//! Session index table page.

use super::escape::{collapse_slashes, encode_repo_url_path, esc, percent_decode};
use super::format::{format_duration_secs, format_relative_age, format_short_age};
use super::layout::wrap_page;
use crate::daemon::model::{sessions_for_index, Session, SessionLifecycle, Store};
use crate::daemon::paths::now_unix_secs;

/// Filtered Projects/Jobs view from `/project/…` or `/repo/…`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexFilter {
  /// Bare project name under `$SCSH_HOME/projects/<name>`.
  Project(String),
  /// Absolute repository path.
  Repo(String),
}

impl IndexFilter {
  /// Absolute repo path this filter matches (projects resolve under `projects_dir`).
  pub fn repo_path(&self) -> String {
    match self {
      Self::Project(name) => crate::daemon::paths::projects_dir().join(name).to_string_lossy().into_owned(),
      Self::Repo(path) => path.clone(),
    }
  }

  pub fn label(&self) -> String {
    match self {
      Self::Project(name) => format!("project · {name}"),
      Self::Repo(path) => path.clone(),
    }
  }
}

/// Parse `/project/…` or `/repo/…` (extra slashes allowed). `None` for bare `/project` / `/repo`.
pub fn parse_index_filter(path: &str) -> Option<IndexFilter> {
  let path = path.split('?').next().unwrap_or(path);
  if let Some(rest) = path.strip_prefix("/project") {
    let name = collapse_slashes(&percent_decode(rest)).trim_matches('/').to_string();
    if name.is_empty() || name.contains('/') {
      return None;
    }
    return Some(IndexFilter::Project(name));
  }
  if let Some(rest) = path.strip_prefix("/repo") {
    let mut p = collapse_slashes(&percent_decode(rest));
    if p.is_empty() || p == "/" {
      return None;
    }
    if !p.starts_with('/') {
      p.insert(0, '/');
    }
    return Some(IndexFilter::Repo(p));
  }
  None
}

/// Which index tab is active on first paint (path-based: `/`, `/jobs`, `/projects`, `/setup`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexTab {
  Run,
  Jobs,
  Projects,
  Stats,
  Setup,
}

impl IndexTab {
  /// Parse a request path (`/`, `/run`, `/jobs`, `/projects`, `/setup`, `/images`).
  pub fn from_path(path: &str) -> Option<Self> {
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    match path {
      "/" | "/run" => Some(Self::Run),
      "/jobs" => Some(Self::Jobs),
      "/projects" => Some(Self::Projects),
      "/stats" => Some(Self::Stats),
      "/setup" | "/images" => Some(Self::Setup),
      _ => None,
    }
  }

  fn crumb(self) -> Option<(&'static str, &'static str)> {
    match self {
      Self::Run => None,
      Self::Jobs => Some(("/jobs", "jobs")),
      Self::Projects => Some(("/projects", "projects")),
      Self::Stats => Some(("/stats", "stats")),
      Self::Setup => Some(("/setup", "setup")),
    }
  }
}

pub fn index_page(store: &Store) -> String {
  index_page_for(store, None, IndexTab::Run)
}

pub fn index_page_with_filter(store: &Store, filter: Option<IndexFilter>) -> String {
  // Filtered /project|/repo URLs open the Projects tab.
  index_page_for(store, filter, IndexTab::Projects)
}

/// Rows shown in the Jobs table before the "Show N more" button. The store holds up to two
/// hundred sessions and the list is running-first, so everything below the first page is
/// history. Mirrored by `JOBS_PAGE` in the client JS.
pub(crate) const JOBS_PAGE_SIZE: usize = 50;

pub fn index_page_for(store: &Store, filter: Option<IndexFilter>, tab: IndexTab) -> String {
  let port = store.port;
  let now = now_unix_secs();
  let filter_repo = filter.as_ref().map(|f| f.repo_path());
  let sessions = sessions_for_index(&store.sessions, now);
  let listed: Vec<&Session> =
    sessions.into_iter().filter(|s| filter_repo.as_ref().is_none_or(|want| &s.repo == want)).collect();
  let mut rows = String::new();
  for (i, session) in listed.iter().enumerate() {
    rows.push_str(&index_session_row(session, now, i >= JOBS_PAGE_SIZE));
  }
  let hidden = listed.len().saturating_sub(JOBS_PAGE_SIZE);
  if hidden > 0 {
    rows.push_str(&jobs_load_more_row(hidden));
  }
  if rows.is_empty() {
    rows = if filter.is_some() {
      "<tr><td colspan=\"7\" class=\"dim\">No jobs for this project or repository.</td></tr>\n".into()
    } else {
      "<tr><td colspan=\"7\" class=\"dim\">No jobs yet — run <code>scsh run</code> to start one.</td></tr>\n".into()
    };
  }
  let active = |want: IndexTab| if tab == want { " active" } else { "" };
  // ARIA tab pattern (server side): tablist/tab/tabpanel roles plus a roving tabindex,
  // so the markup is honest before the client script re-asserts the same state on
  // activation. New attributes stay clear of the `class= data-tab=` pair — tests and
  // the live renderer match on that exact adjacency.
  let selected = |want: IndexTab| if tab == want { "true" } else { "false" };
  let tabindex = |want: IndexTab| if tab == want { "0" } else { "-1" };
  let body = format!(
    "<nav class=\"tabs\" role=\"tablist\">\
<button id=\"tabbtn-run\" role=\"tab\" aria-selected=\"{run_s}\" aria-controls=\"tab-run\" \
tabindex=\"{run_i}\" class=\"tab{run_a}\" data-tab=\"run\">Run</button>\
<button id=\"tabbtn-jobs\" role=\"tab\" aria-selected=\"{jobs_s}\" aria-controls=\"tab-jobs\" \
tabindex=\"{jobs_i}\" class=\"tab{jobs_a}\" data-tab=\"jobs\">Jobs</button>\
<button id=\"tabbtn-projects\" role=\"tab\" aria-selected=\"{proj_s}\" aria-controls=\"tab-projects\" \
tabindex=\"{proj_i}\" class=\"tab{proj_a}\" data-tab=\"projects\">Projects</button>\
<button id=\"tabbtn-stats\" role=\"tab\" aria-selected=\"{stats_s}\" aria-controls=\"tab-stats\" \
tabindex=\"{stats_i}\" class=\"tab{stats_a}\" data-tab=\"stats\">Stats</button>\
<button id=\"tabbtn-setup\" role=\"tab\" aria-selected=\"{setup_s}\" aria-controls=\"tab-setup\" \
tabindex=\"{setup_i}\" class=\"tab{setup_a}\" data-tab=\"setup\">Setup</button>\
</nav>\n\
<section class=\"tab-panel{run_p}\" id=\"tab-run\" role=\"tabpanel\" aria-labelledby=\"tabbtn-run\">\n{start}</section>\n\
<section class=\"tab-panel{jobs_p}\" id=\"tab-jobs\" role=\"tabpanel\" aria-labelledby=\"tabbtn-jobs\">\n\
<div class=\"chamfer card card--accent-left-cyan\">\n\
<p class=\"section-label\">Jobs</p>\n{harness_stops}\
<div class=\"table-scroll\"><table>\n\
<thead><tr><th>Job</th><th>Status</th><th>Started</th><th>Duration</th>\
<th>Profile</th><th>Procs</th><th>Repo</th></tr></thead>\n\
<tbody id=\"sessions-body\">\n{rows}</tbody>\n</table></div>\n\
</div>\n\
</section>\n\
<section class=\"tab-panel{proj_p}\" id=\"tab-projects\" role=\"tabpanel\" aria-labelledby=\"tabbtn-projects\">\n{dirs}</section>\n\
<section class=\"tab-panel{stats_p}\" id=\"tab-stats\" role=\"tabpanel\" aria-labelledby=\"tabbtn-stats\">\n{stats}</section>\n\
<section class=\"tab-panel{setup_p}\" id=\"tab-setup\" role=\"tabpanel\" aria-labelledby=\"tabbtn-setup\">\n{images}</section>\n",
    run_a = active(IndexTab::Run),
    jobs_a = active(IndexTab::Jobs),
    proj_a = active(IndexTab::Projects),
    stats_a = active(IndexTab::Stats),
    setup_a = active(IndexTab::Setup),
    run_s = selected(IndexTab::Run),
    jobs_s = selected(IndexTab::Jobs),
    proj_s = selected(IndexTab::Projects),
    stats_s = selected(IndexTab::Stats),
    setup_s = selected(IndexTab::Setup),
    run_i = tabindex(IndexTab::Run),
    jobs_i = tabindex(IndexTab::Jobs),
    proj_i = tabindex(IndexTab::Projects),
    stats_i = tabindex(IndexTab::Stats),
    setup_i = tabindex(IndexTab::Setup),
    run_p = active(IndexTab::Run),
    jobs_p = active(IndexTab::Jobs),
    proj_p = active(IndexTab::Projects),
    stats_p = active(IndexTab::Stats),
    setup_p = active(IndexTab::Setup),
    rows = rows,
    harness_stops = harness_stop_strip(store, now),
    dirs = dirs_panel(store, now, filter.as_ref()),
    start = start_panel(),
    stats = super::stats::stats_panel(),
    images = images_panel()
  );
  wrap_page("scsh", port, None, tab.crumb(), "", &body)
}

/// One red "✕ stop all <harness> (n)" button per harness with running skill containers, so a
/// misbehaving harness (say, grok out of quota) can be cut across every live session at once
/// (`POST /api/v1/harness/stop`). Empty when nothing is running.
fn harness_stop_strip(store: &Store, now: u64) -> String {
  use crate::daemon::model::{ProcKind, ProcStatus};
  let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
  let mut terminating: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
  for s in store.sessions.values() {
    // Lifecycle, not `ended_at`: a client that died without deregistering leaves its session
    // un-ended (and its procs "running") FOREVER — those are Terminated zombies, and offering
    // to stop their long-gone containers is noise. Only genuinely live sessions count.
    if s.lifecycle_status(now) != SessionLifecycle::Running {
      continue;
    }
    for p in &s.procs {
      let live = p.status == ProcStatus::Running || p.status == ProcStatus::Waiting;
      if live && p.kind == ProcKind::Skill {
        if let Some(h) = p.harness.as_deref().filter(|h| !h.is_empty()) {
          let target = if matches!(
            p.fail_reason.as_deref(),
            Some(crate::failure::reason::STOP_REQUESTED) | Some(crate::failure::reason::RESTART_REQUESTED)
          ) {
            &mut terminating
          } else {
            &mut counts
          };
          *target.entry(h.to_string()).or_insert(0) += 1;
        }
      }
    }
  }
  if counts.is_empty() && terminating.is_empty() {
    return String::new();
  }
  let mut buttons = String::new();
  for (harness, n) in &terminating {
    buttons.push_str(&format!(
      "<button type=\"button\" class=\"chamfer btn btn--orange btn--sm\" disabled title=\"Stopping every running {h} container, in every job\"><span>Terminating all {h} ({n})…</span></button>\n",
      h = esc(harness),
    ));
  }
  for (harness, n) in &counts {
    buttons.push_str(&format!(
      "<button type=\"button\" class=\"chamfer btn btn--red btn--sm\" data-harness-stop=\"{h}\" title=\"Stop every running {h} container, in every job\"><span>✕ stop all {h} ({n})</span></button>\n",
      h = esc(harness),
    ));
  }
  format!("<div class=\"harness-stops\">{buttons}</div>\n")
}

/// The Setup panel: per-harness readiness (image + login), with the low-level image
/// inventory under a collapsed Advanced disclosure. Populated by `GET /api/v1/setup`.
///
/// First paint already lists every harness in a `checking…` state (§13: no empty limbo
/// while the runtime inspect runs). Path `/setup` (and `/images` as a compatibility alias).
fn images_panel() -> String {
  let mut cards = String::new();
  for h in crate::config::Harness::ALL {
    cards.push_str(&setup_skeleton_card(h));
  }
  let mut rows = String::new();
  rows.push_str(&images_skeleton_row("base", crate::runtime::BASE_IMAGE_TAG, true));
  for h in crate::config::Harness::ALL {
    rows.push_str(&images_skeleton_row(h.as_str(), &crate::runtime::image_tag(h), true));
  }
  format!(
    r##"<div class="chamfer card card--accent-left-orange">
<p class="section-label">Harness setup</p>
<p class="dim">Prepare and verify the agents scsh can run. Choose a runtime, build missing
images, and sign in on the host. Image freshness alone is not readiness — login is checked
separately. Model probes run only when you click <strong>Test</strong> (real provider calls).</p>
<div class="setup-toolbar">
<div id="images-runtimes" class="images-runtimes"></div>
<span id="setup-checked" class="dim"></span>
<a href="#" id="setup-refresh">refresh</a>
<button type="button" class="chamfer btn btn--cyan btn--sm" id="setup-test-all" title="One primary smoke model per ready harness"><span>Test all defaults</span></button>
</div>
<div id="setup-engine" class="chamfer blockers" hidden></div>
<p id="setup-summary" class="setup-summary dim">checking agents…</p>
<div id="setup-cards" class="setup-cards">{cards}</div>
</div>
<div class="chamfer card card--accent-left-cyan">
<p class="section-label">Subscription quota</p>
<p class="dim">Live usage from each provider's own endpoint, using the host logins above.
Read-only — no model calls, no cost. Checking runs a real job (one run per harness, each
with its own result file), started only when you click. Also available as
<code>scsh quota [--json]</code>.</p>
<div class="setup-toolbar">
<button type="button" class="chamfer btn btn--cyan btn--sm" id="setup-quota-btn" title="Query each provider's usage endpoint with the host logins (read-only)"><span>Check quota</span></button>
<span id="setup-quota-note" class="dim"></span>
</div>
<div class="table-scroll"><table id="setup-quota-table" hidden>
<thead><tr><th>Harness</th><th>Plan</th><th>Window</th><th>Used</th><th>Resets (UTC)</th></tr></thead>
<tbody id="setup-quota-body"></tbody>
</table></div>
</div>
<div class="chamfer card card--accent-left-purple">
<p class="section-label">Images setup</p>
<p class="dim">Image tags, sizes, timestamps, base rebuilds, and force rebuilds for the
selected runtime.</p>
<div class="table-scroll"><table>
<thead><tr><th></th><th>Image</th><th>Status</th><th>Created</th><th>Size</th><th></th></tr></thead>
<tbody id="images-body">
{rows}</tbody>
</table></div>
<div class="images-controls">
<button type="button" class="chamfer btn btn--cyan btn--sm" id="images-build-selected" disabled><span>Build selected</span></button>
<button type="button" class="chamfer btn btn--orange btn--sm" id="images-build-stale"><span>Build stale</span></button>
<button type="button" class="chamfer btn btn--purple btn--sm" id="images-build-all"><span>Build all</span></button>
<label><input type="checkbox" id="images-rebuild-base"> also rebuild the base image (--no-cache)</label>
<label><input type="checkbox" id="images-force"> force rebuild even when up to date</label>
<a href="#" id="images-refresh">refresh</a>
<span id="images-note" class="dim">checking container runtime…</span>
</div>
</div>
"##,
    cards = cards,
    rows = rows
  )
}

fn setup_skeleton_card(h: crate::config::Harness) -> String {
  format!(
    r#"<article class="chamfer setup-card" data-harness="{id}" data-pending="1">
<header class="setup-card-head">
<strong class="setup-card-name">{name}</strong>
<span class="chamfer session-status checking"><span>checking…</span></span>
</header>
<div class="setup-card-layers">
<div><span class="setup-layer-label">Image</span> <span class="setup-layer-value dim">checking…</span></div>
<div><span class="setup-layer-label">Login</span> <span class="setup-layer-value dim">checking…</span></div>
</div>
<ul class="setup-models dim"><li class="setup-models-hint">Check models, then <strong>Test selected</strong></li></ul>
<div class="setup-card-actions"><span class="dim setup-next">Real container probe — may incur provider cost</span></div>
</article>
"#,
    id = esc(h.as_str()),
    name = esc(h.display_name()),
  )
}

/// One known-image row in its first observable state: present, status `checking…`.
fn images_skeleton_row(name: &str, tag: &str, selectable: bool) -> String {
  let checkbox = if selectable {
    format!(r#"<input type="checkbox" class="image-select" value="{name}" disabled>"#, name = esc(name))
  } else {
    String::new()
  };
  format!(
    r#"<tr data-image="{name}" data-pending="1"><td class="image-select-cell">{checkbox}</td><td><code>{tag}</code></td><td class="image-status-cell"><span class="chamfer session-status checking"><span>checking…</span></span></td><td class="dim image-created-cell">—</td><td class="dim image-size-cell">—</td><td class="image-action-cell"></td></tr>
"#,
    name = esc(name),
    tag = esc(tag),
    checkbox = checkbox
  )
}

/// The Run tab: open a git repo (POST `/api/v1/repos/open`, which reports whether it is
/// runnable and why not), pick a harness definition, fill the rendered param form, and start a
/// job (POST `/api/v1/jobs/start`, deep-linking to the spawned session). Start is disabled until
/// the repo is runnable. Default landing tab on the index page.
fn start_panel() -> &'static str {
  r##"<div class="chamfer card card--accent-left-green">
<p class="section-label">Run</p>
<p class="dim">Open a git repository — an absolute path, or the bare name of a project under
<code>~/.scsh/projects/</code> — to configure and start a harness-definition job in it; the
daemon runs it just like <code>scsh run</code>. The repo must be committed, clean, and have a
gitignored scratch dir (<code>tmp/</code> or <code>.harness/tmp</code>). One job per repository at a time.</p>
<p class="dim">Or <strong>create a new project</strong>: a fresh git repository under
<code>~/.scsh/projects/&lt;name&gt;</code>, born runnable (its first commit gitignores
<code>/tmp</code>) — tests and demos start right here, no terminal needed.</p>
<div class="start-controls">
<div class="chamfer input-wrap">
<input class="input" type="text" id="repo-path" placeholder="/path/to/a/git/repo, or a project name (type, paste, or Pick…)">
</div>
<div class="start-actions">
<button type="button" class="chamfer btn btn--purple btn--sm" id="repo-pick"><span>Pick…</span></button>
<button type="button" class="chamfer btn btn--cyan btn--sm" id="repo-open"><span>Open</span></button>
</div>
<span id="repo-note" class="dim"></span>
</div>
<div class="start-controls">
<div class="chamfer input-wrap">
<input class="input" type="text" id="project-name" placeholder="project name (letters, digits, - or _; no dots/slashes)" autocomplete="off" spellcheck="false">
</div>
<div class="start-actions">
<button type="button" class="chamfer btn btn--green btn--sm" id="project-create"><span>New Project</span></button>
</div>
</div>
<div id="repo-blockers" class="chamfer blockers" hidden></div>
<div id="defs-panel" hidden>
<p class="section-label">Definitions</p>
<p class="dim">Harness definitions in <code id="open-repo-path"></code> — pick one to configure and start.</p>
<div id="defs-list"></div>
<div id="def-form"></div>
</div>
</div>
"##
}

/// The "Projects" tab: every job the daemon knows about, grouped by the repository it runs
/// in (plus repositories opened from the UI that have no jobs yet). Rendered server-side so
/// the tab is populated on first paint; `renderRepoJobs` in the client JS re-renders the
/// same markup from live tick snapshots — keep the two byte-identical.
fn dirs_panel(store: &Store, now: u64, filter: Option<&IndexFilter>) -> String {
  let banner = match filter {
    Some(f) => format!(
      "<p class=\"chamfer filter-banner\" data-repo-filter=\"{path}\">Showing <strong>{label}</strong> · \
<a class=\"filter-clear\" href=\"/projects\">Show all</a></p>\n",
      path = esc(&f.repo_path()),
      label = esc(&f.label()),
    ),
    None => String::new(),
  };
  format!(
    r##"<div class="chamfer card card--accent-left-magenta">
<p class="section-label">Projects</p>
{banner}<p class="dim">Current jobs, grouped by where they run: a project under <code>~/.scsh/projects/</code> shows its
name; anything else shows its repository path. Click a name to filter.</p>
<div class="table-scroll"><table>
<thead><tr><th>Project / repository</th><th>Jobs</th></tr></thead>
<tbody id="repos-body">{rows}</tbody>
</table></div>
</div>
{internal}
"##,
    banner = banner,
    rows = repo_jobs_rows(store, now, filter),
    internal = internal_panel(store, now, filter),
  )
}

/// Projects → Internal: synthetic-repo sessions (`(image builds)`, `(internal)`), grouped by
/// profile. Hidden when filtering to a real project/repo or when none exist. Mirrored by
/// `renderInternalJobs` in the client JS.
fn internal_panel(store: &Store, now: u64, filter: Option<&IndexFilter>) -> String {
  if filter.is_some() {
    return String::new();
  }
  let mut jobs: Vec<&Session> =
    store.sessions.values().filter(|s| s.parent_session.is_none() && is_internal_repo(&s.repo)).collect();
  if jobs.is_empty() {
    return String::new();
  }
  let activity = |s: &Session| s.ended_at.unwrap_or(s.started_at);
  let mut groups: std::collections::BTreeMap<&str, Vec<&Session>> = std::collections::BTreeMap::new();
  for s in jobs.drain(..) {
    groups.entry(s.profile.as_deref().unwrap_or("default")).or_default().push(s);
  }
  let mut ordered: Vec<(&str, Vec<&Session>)> = groups.into_iter().collect();
  for (_, g) in &mut ordered {
    g.sort_by_key(|s| {
      (std::cmp::Reverse(s.lifecycle_status(now) == SessionLifecycle::Running), std::cmp::Reverse(activity(s)))
    });
  }
  ordered.sort_by_key(|(_, g)| {
    (
      std::cmp::Reverse(g.iter().any(|s| s.lifecycle_status(now) == SessionLifecycle::Running)),
      std::cmp::Reverse(g.iter().map(|s| activity(s)).max().unwrap_or(0)),
    )
  });
  let body = ordered
    .iter()
    .map(|(task, g)| {
      let links = g
        .iter()
        .map(|s| {
          let lc = s.lifecycle_status(now);
          format!(
            "<div class=\"repo-job\"><span class=\"chamfer session-status {cls}\"><span>{label}</span></span> <a class=\"job-id\" href=\"/job/{id}\">{id}</a> <span class=\"dim\">{age}</span></div>",
            id = esc(&s.id),
            cls = lc.css_class(),
            label = lc.label(),
            age = format_short_age(now.saturating_sub(activity(s))),
          )
        })
        .collect::<Vec<_>>()
        .join("");
      format!(
        "<div class=\"repo-jobgroup\"><span class=\"repo-jobgroup-name\">{task}</span>{links}</div>",
        task = esc(task),
        links = links,
      )
    })
    .collect::<Vec<_>>()
    .join("");
  format!(
    r##"<div class="chamfer card card--accent-left-purple" id="internal-jobs-card">
<p class="section-label">Internal</p>
<p class="dim">System jobs — image builds and annotate catch-up — not tied to a project or repository.</p>
<div id="internal-body">{body}</div>
</div>
"##,
    body = body,
  )
}

fn is_internal_repo(repo: &str) -> bool {
  repo == crate::daemon::server::IMAGE_BUILDS_REPO || repo == crate::daemon::server::INTERNAL_REPO
}

/// Rows of the Projects table. Within a repository the jobs are grouped by the task they
/// ran (the workflow/profile name), running groups above finished ones, newest first, each
/// job with a compact age stamp. Mirrored by `renderRepoJobs` in the client JS
/// (`client_js.rs`) — keep the markup identical.
fn repo_jobs_rows(store: &Store, now: u64, filter: Option<&IndexFilter>) -> String {
  let filter_repo = filter.map(|f| f.repo_path());
  let mut by_repo: std::collections::BTreeMap<&str, Vec<&Session>> = std::collections::BTreeMap::new();
  for path in store.open_repos.keys() {
    if filter_repo.as_ref().is_some_and(|want| path != want) {
      continue;
    }
    by_repo.entry(path).or_default();
  }
  for s in store.sessions.values() {
    if s.parent_session.is_some() || s.repo.is_empty() || is_internal_repo(&s.repo) {
      continue;
    }
    if filter_repo.as_ref().is_some_and(|want| &s.repo != want) {
      continue;
    }
    by_repo.entry(&s.repo).or_default().push(s);
  }
  if by_repo.is_empty() {
    return if filter.is_some() {
      "<tr><td colspan=\"2\" class=\"dim\">No jobs for this project or repository.</td></tr>".into()
    } else {
      "<tr><td colspan=\"2\" class=\"dim\">No jobs yet — open or create a project under Run.</td></tr>".to_string()
    };
  }
  // A job's "activity" moment: when it finished, or when it started if still going.
  let activity = |s: &Session| s.ended_at.unwrap_or(s.started_at);
  let projects_root = format!("{}/", crate::daemon::paths::projects_dir().display());
  let mut rows = String::new();
  for (repo, jobs) in by_repo {
    let cells = if jobs.is_empty() {
      "<span class=\"dim\">no jobs yet</span>".to_string()
    } else {
      let mut groups: std::collections::BTreeMap<&str, Vec<&Session>> = std::collections::BTreeMap::new();
      for s in jobs {
        groups.entry(s.profile.as_deref().unwrap_or("default")).or_default().push(s);
      }
      let mut ordered: Vec<(&str, Vec<&Session>)> = groups.into_iter().collect();
      for (_, g) in &mut ordered {
        g.sort_by_key(|s| {
          (std::cmp::Reverse(s.lifecycle_status(now) == SessionLifecycle::Running), std::cmp::Reverse(activity(s)))
        });
      }
      // Groups with something running come first, then by most recent activity.
      ordered.sort_by_key(|(_, g)| {
        (
          std::cmp::Reverse(g.iter().any(|s| s.lifecycle_status(now) == SessionLifecycle::Running)),
          std::cmp::Reverse(g.iter().map(|s| activity(s)).max().unwrap_or(0)),
        )
      });
      ordered
        .iter()
        .map(|(task, g)| {
          let links = g
            .iter()
            .map(|s| {
              let lc = s.lifecycle_status(now);
              format!(
                "<div class=\"repo-job\"><span class=\"chamfer session-status {cls}\"><span>{label}</span></span> <a class=\"job-id\" href=\"/job/{id}\">{id}</a> <span class=\"dim\">{age}</span></div>",
                id = esc(&s.id),
                cls = lc.css_class(),
                label = lc.label(),
                age = format_short_age(now.saturating_sub(activity(s))),
              )
            })
            .collect::<Vec<_>>()
            .join("");
          format!(
            "<div class=\"repo-jobgroup\"><span class=\"repo-jobgroup-name\">{task}</span>{links}</div>",
            task = esc(task),
            links = links,
          )
        })
        .collect::<Vec<_>>()
        .join("")
    };
    let href = repo_filter_href(repo, &projects_root);
    rows.push_str(&format!(
      "<tr data-repo=\"{repo}\"><td class=\"repo-path\" title=\"{repo}\"><a class=\"repo-filter-link\" href=\"{href}\">{label}</a></td><td>{cells}</td></tr>",
      repo = esc(repo),
      href = esc(&href),
      label = esc(&repo_display_label(repo, &projects_root)),
      cells = cells,
    ));
  }
  rows
}

/// `/project/<name>` for scsh projects, `/repo/<abs-path>` for everything else.
fn repo_filter_href(repo: &str, projects_root: &str) -> String {
  match repo.strip_prefix(projects_root) {
    Some(name) if !name.is_empty() && !name.contains('/') => format!("/project/{name}"),
    _ => format!("/repo{}", encode_repo_url_path(repo)),
  }
}

/// A repo under `~/.scsh/projects/` displays as `project · <name>`; anything else shows its
/// full path. Mirrors `repoLabel` inside `renderRepoJobs` in the client JS.
fn repo_display_label(repo: &str, projects_root: &str) -> String {
  match repo.strip_prefix(projects_root) {
    Some(name) => format!("project · {name}"),
    None => repo.to_string(),
  }
}

/// The tbody row carrying the "Show N more" button when the Jobs table overflows its first
/// page. Clicking reveals the next page of `jobs-overflow` rows in place — the rows are all
/// served, only unrevealed. Mirrored byte-for-byte by `jobsLoadMoreRowHtml` in the client JS.
fn jobs_load_more_row(hidden: usize) -> String {
  let step = hidden.min(JOBS_PAGE_SIZE);
  let of = if hidden > step { format!(" of {hidden}") } else { String::new() };
  format!(
    "<tr class=\"jobs-more-row\"><td colspan=\"7\">\
<button type=\"button\" class=\"chamfer btn btn--cyan btn--sm jobs-load-more\">\
<span>Show {step} more{of}</span></button></td></tr>\n"
  )
}

fn index_session_row(session: &Session, now: u64, overflow: bool) -> String {
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
    "<tr{overflow} data-session-id=\"{id}\"><td><a class=\"job-id\" href=\"/job/{id}\">{id}</a></td>\
<td class=\"session-status-cell\">{status}</td>\
<td class=\"session-started-cell\">{started}</td>\
<td class=\"session-duration-cell\">{duration}</td>\
<td>{profile}</td><td class=\"session-procs-cell\"><span class=\"chip-count\" data-tip=\"{n_procs} run{plural} in this job\">{n_procs}</span>{chips}</td><td class=\"dim repo-path session-repo-path\"><button type=\"button\" class=\"repo-copy\" data-copy-value=\"{repo}\" data-tip=\"{repo}\" aria-label=\"Copy full repository path\">{repo}</button></td></tr>\n",
    overflow = if overflow { " class=\"jobs-overflow\"" } else { "" },
    id = id,
    status = status,
    started = started,
    duration = esc(&duration),
    profile = profile,
    chips = harness_chips_html(session),
    n_procs = n_procs,
    plural = if n_procs == 1 { "" } else { "s" },
    repo = esc(&session.repo),
  )
}

/// Colored single-letter harness chips, one per skill proc — C in Anthropic orange is claude,
/// C in green is codex, C in violet is cursor — so a row shows at a glance what it runs and
/// what is still running (finished chips dim out). The tooltip is two lines — `harness ·
/// skill` then the status; a running chip instead carries `data-tip-running` (its start
/// time), from which the tip module composes a live-ticking "running for …" line without
/// churning the markup every second. Mirrored byte-for-byte by `harnessChipsHtml` in the
/// client JS (`client_js.rs`).
fn harness_chips_html(session: &Session) -> String {
  use crate::daemon::model::{ProcKind, ProcStatus};
  const MAX_CHIPS: usize = 8;
  let mut chips = String::new();
  let skill_procs: Vec<_> = session
    .procs
    .iter()
    .filter(|p| p.kind == ProcKind::Skill)
    .filter_map(|p| p.harness.as_deref().filter(|h| !h.is_empty()).map(|h| (p, h)))
    .collect();
  for (p, h) in skill_procs.iter().take(MAX_CHIPS).copied() {
    let letter = h.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('?');
    let done = matches!(p.status, ProcStatus::Ok | ProcStatus::Graceful | ProcStatus::Fail | ProcStatus::Skipped);
    let skill = p.skill_name.as_deref().unwrap_or(&p.label);
    let base = format!("{h} · {skill}");
    let (tip, running_attr) = match p.status {
      ProcStatus::Running => match p.started_at {
        Some(t) => (base, format!(" data-tip-running=\"{t}\"")),
        None => (format!("{base}\nrunning"), String::new()),
      },
      ProcStatus::Waiting => (format!("{base}\nwaiting"), String::new()),
      ProcStatus::Ok => (format!("{base}\ndone"), String::new()),
      ProcStatus::Graceful => (format!("{base}\ngraceful shutdown"), String::new()),
      ProcStatus::Fail => (format!("{base}\nfailed"), String::new()),
      ProcStatus::Skipped => (format!("{base}\nskipped"), String::new()),
    };
    let fragment = match super::workflow::proc_task_id(session, p) {
      Some(step) => format!("#task-{}", esc(&step)),
      None => format!("#proc-{}", p.index),
    };
    chips.push_str(&format!(
      "<a class=\"chamfer hchip hchip--{h}{done}\" href=\"/job/{session_id}{fragment}\" data-tip=\"{tip}\"{running_attr}>{letter}</a>",
      h = esc(h),
      done = if done { " hchip--done" } else { "" },
      session_id = esc(&session.id),
      tip = esc(&tip),
    ));
  }
  if skill_procs.len() > MAX_CHIPS {
    chips.push_str(&format!("<span class=\"chip-overflow\">+ {}</span>", skill_procs.len() - MAX_CHIPS));
  }
  chips
}

fn index_duration_label(session: &Session, now: u64, lifecycle: SessionLifecycle) -> String {
  match session.duration_secs(now) {
    Some(secs) if lifecycle == SessionLifecycle::Running => format!("{} so far", format_duration_secs(secs)),
    Some(secs) => format_duration_secs(secs),
    None => "—".to_string(),
  }
}
