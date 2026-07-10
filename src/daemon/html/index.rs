//! Session index table page.

use super::escape::esc;
use super::format::{format_duration_secs, format_relative_age, format_short_age};
use super::layout::wrap_page;
use crate::daemon::model::{sessions_for_index, Session, SessionLifecycle, Store};
use crate::daemon::paths::now_unix_secs;

pub fn index_page(store: &Store) -> String {
  let port = store.port;
  let now = now_unix_secs();
  let sessions = sessions_for_index(&store.sessions, now);
  let mut rows = String::new();
  for session in sessions {
    rows.push_str(&index_session_row(session, now));
  }
  if rows.is_empty() {
    rows = "<tr><td colspan=\"7\" class=\"dim\">No jobs yet — run <code>scsh run</code> to start one.</td></tr>\n"
      .to_string();
  }
  let body = format!(
    "<nav class=\"tabs\">\
<button class=\"tab active\" data-tab=\"jobs\">Jobs</button>\
<button class=\"tab\" data-tab=\"dirs\">Projects</button>\
<button class=\"tab\" data-tab=\"start\">New job</button>\
<button class=\"tab\" data-tab=\"images\">Containers</button>\
</nav>\n\
<section class=\"tab-panel active\" id=\"tab-jobs\">\n\
<div class=\"card card--accent-left-cyan\">\n\
<p class=\"section-label\">Jobs</p>\n{harness_stops}\
<div class=\"table-scroll\"><table>\n\
<thead><tr><th>Job</th><th>Status</th><th>Started</th><th>Duration</th>\
<th>Profile</th><th>Procs</th><th>Repo</th></tr></thead>\n\
<tbody id=\"sessions-body\">\n{rows}</tbody>\n</table></div>\n\
</div>\n\
</section>\n\
<section class=\"tab-panel\" id=\"tab-dirs\">\n{dirs}</section>\n\
<section class=\"tab-panel\" id=\"tab-start\">\n{start}</section>\n\
<section class=\"tab-panel\" id=\"tab-images\">\n{images}</section>\n",
    rows = rows,
    harness_stops = harness_stop_strip(store, now),
    dirs = dirs_panel(store, now),
    start = start_panel(),
    images = images_panel()
  );
  wrap_page("scsh", port, None, &body)
}

/// One red "✕ stop all <harness> (n)" button per harness with running skill containers, so a
/// misbehaving harness (say, grok out of quota) can be cut across every live session at once
/// (`POST /api/v1/harness/stop`). Empty when nothing is running.
fn harness_stop_strip(store: &Store, now: u64) -> String {
  use crate::daemon::model::{ProcKind, ProcStatus};
  let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
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
          *counts.entry(h.to_string()).or_insert(0) += 1;
        }
      }
    }
  }
  if counts.is_empty() {
    return String::new();
  }
  let mut buttons = String::new();
  for (harness, n) in &counts {
    buttons.push_str(&format!(
      "<button type=\"button\" class=\"chamfer btn btn--red btn--sm\" data-harness-stop=\"{h}\" title=\"Stop every running {h} container, in every job\"><span>✕ stop all {h} ({n})</span></button>\n",
      h = esc(harness),
    ));
  }
  format!("<div class=\"harness-stops\">{buttons}</div>\n")
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
  rows.push_str(&images_skeleton_row("base", crate::runtime::BASE_IMAGE_TAG, true));
  for h in crate::config::Harness::ALL {
    rows.push_str(&images_skeleton_row(h.as_str(), &crate::runtime::image_tag(h), true));
  }
  format!(
    r##"<div class="card card--accent-left-orange">
<p class="section-label">Containers</p>
<p class="dim">The base container images scsh builds: the shared base, plus one per harness.
Stale means the image exists but no longer matches this scsh build's embedded Dockerfile —
rebuild it here.</p>
<div id="images-runtimes" class="images-runtimes"></div>
<div class="table-scroll"><table>
<thead><tr><th></th><th>Image</th><th>Status</th><th>Created</th><th>Size</th><th></th></tr></thead>
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

/// The "New job" tab: open a git repo (POST `/api/v1/repos/open`, which reports whether it is
/// runnable and why not), pick a harness definition, fill the rendered param form, and start a
/// job (POST `/api/v1/jobs/start`, deep-linking to the spawned session). Start is disabled until
/// the repo is runnable.
fn start_panel() -> &'static str {
  r##"<div class="card card--accent-left-green">
<p class="section-label">New job</p>
<p class="dim">Open a git repository — an absolute path, or the bare name of a project under
<code>~/.scsh/projects/</code> — to configure and start a harness-definition job in it; the
daemon runs it just like <code>scsh run</code>. The repo must be committed, clean, and have a
gitignored scratch dir (<code>tmp/</code> or <code>.harness/tmp</code>). One job per repository at a time.
Or <strong>create a new project</strong>: a fresh git repository under
<code>~/.scsh/projects/&lt;name&gt;</code>, born runnable (its first commit gitignores
<code>/tmp</code>) — tests and demos start right here, no terminal needed.</p>
<div class="images-controls">
<div class="chamfer input-wrap" style="flex:1;min-width:16rem">
<input class="input" type="text" id="repo-path" placeholder="/path/to/a/git/repo, or a project name (type, paste, or Pick…)">
</div>
<button type="button" class="chamfer btn btn--purple btn--sm" id="repo-pick"><span>Pick…</span></button>
<button type="button" class="chamfer btn btn--cyan btn--sm" id="repo-open"><span>Open</span></button>
<span id="repo-note" class="dim"></span>
</div>
<div class="images-controls">
<div class="chamfer input-wrap" style="flex:1;min-width:16rem">
<input class="input" type="text" id="project-name" placeholder="new project name (created under ~/.scsh/projects/)">
</div>
<button type="button" class="chamfer btn btn--green btn--sm" id="project-create"><span>New project</span></button>
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

/// The "Projects" tab: every job the daemon knows about, grouped by the repository it runs
/// in (plus repositories opened from the UI that have no jobs yet). Rendered server-side so
/// the tab is populated on first paint; `renderRepoJobs` in the client JS re-renders the
/// same markup from live tick snapshots — keep the two byte-identical.
fn dirs_panel(store: &Store, now: u64) -> String {
  format!(
    r##"<div class="card card--accent-left-magenta">
<p class="section-label">Projects</p>
<p class="dim">Current jobs, grouped by where they run: a project under <code>~/.scsh/projects/</code> shows its
name; anything else shows its repository path.</p>
<div class="table-scroll"><table>
<thead><tr><th>Project / repository</th><th>Jobs</th></tr></thead>
<tbody id="repos-body">{rows}</tbody>
</table></div>
</div>
"##,
    rows = repo_jobs_rows(store, now)
  )
}

/// Rows of the Projects table. Within a repository the jobs are grouped by the task they
/// ran (the workflow/profile name), running groups above finished ones, newest first, each
/// job with a compact age stamp. Mirrored by `renderRepoJobs` in the client JS
/// (`client_js.rs`) — keep the markup identical.
fn repo_jobs_rows(store: &Store, now: u64) -> String {
  let mut by_repo: std::collections::BTreeMap<&str, Vec<&Session>> = std::collections::BTreeMap::new();
  for path in store.open_repos.keys() {
    by_repo.entry(path).or_default();
  }
  for s in store.sessions.values() {
    if s.repo.is_empty() || s.repo == crate::daemon::server::IMAGE_BUILDS_REPO {
      continue;
    }
    by_repo.entry(&s.repo).or_default().push(s);
  }
  if by_repo.is_empty() {
    return "<tr><td colspan=\"2\" class=\"dim\">No jobs yet — open or create a project under “New job”.</td></tr>"
      .to_string();
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
                "<a href=\"/session/{id}\"><span class=\"chamfer session-status {cls}\"><span>{label}</span></span> {id} <span class=\"dim\">{age}</span></a>",
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
    rows.push_str(&format!(
      "<tr><td class=\"repo-path\" title=\"{repo}\">{label}</td><td>{cells}</td></tr>",
      repo = esc(repo),
      label = esc(&repo_display_label(repo, &projects_root)),
      cells = cells,
    ));
  }
  rows
}

/// A repo under `~/.scsh/projects/` displays as `project · <name>`; anything else shows its
/// full path. Mirrors `repoLabel` inside `renderRepoJobs` in the client JS.
fn repo_display_label(repo: &str, projects_root: &str) -> String {
  match repo.strip_prefix(projects_root) {
    Some(name) => format!("project · {name}"),
    None => repo.to_string(),
  }
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
<td>{profile}</td><td class=\"session-procs-cell\">{chips}<span class=\"chip-count\" data-tip=\"{n_procs} run{plural} in this job\">{n_procs}</span></td><td class=\"dim repo-path\">{repo}</td></tr>\n",
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
  let mut chips = String::new();
  for p in &session.procs {
    if p.kind != ProcKind::Skill {
      continue;
    }
    let Some(h) = p.harness.as_deref().filter(|h| !h.is_empty()) else {
      continue;
    };
    let letter = h.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('?');
    let done = matches!(p.status, ProcStatus::Ok | ProcStatus::Fail | ProcStatus::Skipped);
    let skill = p.skill_name.as_deref().unwrap_or(&p.label);
    let base = format!("{h} · {skill}");
    let (tip, running_attr) = match p.status {
      ProcStatus::Running => match p.started_at {
        Some(t) => (base, format!(" data-tip-running=\"{t}\"")),
        None => (format!("{base}\nrunning"), String::new()),
      },
      ProcStatus::Waiting => (format!("{base}\nwaiting"), String::new()),
      ProcStatus::Ok => (format!("{base}\ndone"), String::new()),
      ProcStatus::Fail => (format!("{base}\nfailed"), String::new()),
      ProcStatus::Skipped => (format!("{base}\nskipped"), String::new()),
    };
    chips.push_str(&format!(
      "<span class=\"hchip hchip--{h}{done}\" data-tip=\"{tip}\"{running_attr}>{letter}</span>",
      h = esc(h),
      done = if done { " hchip--done" } else { "" },
      tip = esc(&tip),
    ));
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
