//! Job export: EVERY recording of a job assembled into ONE self-contained offline `.html`
//! page, served at `/job/<id>/export.html` as a download attachment.
//!
//! The page is a REPLICA of the live job page: the same stylesheet (`layout::PAGE_CSS`),
//! the same lede and full meta (ended, duration), the same workflow DAG and fleet
//! comparison sections (state frozen at export time, graph controls still interactive),
//! the same purple island and
//! collapsible per-run rows — text-log procs keep their timestamped lines — and the same
//! `beecast-player` —
//! one shared bundle, one player per recording, mounted from inline data (no iframes, no
//! per-cast page copies). What the live page does over HTTP the export inlines: the cast
//! text, the sidecar summary, and the chapter markers ride in a single JSON block, and a
//! small boot script mounts the players with the exact options the live page uses
//! (`fit: 'both'`, idle compression, chapter markers, player-owned fullscreen,
//! focus-on-open). Live-only machinery — WebSocket, Live toggle, reload, downloads,
//! Force stop — simply is not there (and `LIVE_ONLY_CSS` is not inlined). The commits
//! diffs open the way the live page opens them — as the ENTIRE page, behind the same
//! `⇄ all commits` / `⇄ commits diff` buttons — except that the packdiff pages ride inside
//! the file (one JSON block, like the casts) and the "navigation" is a `#diff-<key>` hash:
//! a full-viewport `srcdoc` frame takes the page over, and packdiff's own Back button (or
//! the browser's) pops the hash and returns to the job. The boot
//! scripts tolerate a copy re-saved from the browser ("Save as"), which serializes the
//! live DOM: mounted players, graph state, and an open diff come back as inert markup and
//! are reset before anything binds. Everything stays a single file. The note under the meta names the scsh version that prepared
//! the file and links it to the crate on crates.io.

use super::escape::esc;
use super::fleet::fleet_sections_by_anchor;
use super::format::format_duration_secs;
use super::layout::{FAVICON_LINK, PAGE_CSS};
use super::proc::{proc_elapsed_phrase, proc_meta_html, proc_note_html, proc_quota_html, proc_usage_html};
use super::session::{session_ended_text, session_lede_html};
use super::workflow::{annotation_label, proc_task_anchor_html, proc_task_attrs, workflow_graph_html_for};
use super::workflow_view_js::WORKFLOW_VIEW_JS;
use crate::daemon::model::{ProcRecord, ReportSection, Session, SessionLifecycle};
use crate::daemon::paths::now_unix_secs;
use crate::json::quote;

/// What the export gathered for one proc (aligned 1:1 with `session.procs`): the raw
/// recording plus its sidecar's summary and chapters, or a note explaining why there is
/// nothing to embed — never an error. Optional packed commits-diff HTML for offline
/// review (same file the live `⇄ commits diff` chip opens). `annotation` is the state of
/// the annotate proc covering this recording when the export was taken (`"ok"`, `"fail"`,
/// `"running"`), or `None` when no annotation was ever registered for it.
pub(crate) enum CastExport {
  Cast {
    ndjson: String,
    summary: Option<String>,
    chapters: Vec<(f64, String)>,
    diff_html: Option<String>,
    annotation: Option<&'static str>,
  },
  /// A headless run: its transcript, already rendered by the same code the live page fetches.
  Transcript {
    html: String,
    diff_html: Option<String>,
  },
  Note {
    text: String,
    diff_html: Option<String>,
  },
}

impl CastExport {
  pub(crate) fn diff_html(&self) -> Option<&str> {
    match self {
      CastExport::Cast { diff_html, .. }
      | CastExport::Transcript { diff_html, .. }
      | CastExport::Note { diff_html, .. } => diff_html.as_deref(),
    }
  }

  fn annotation(&self) -> Option<&'static str> {
    match self {
      CastExport::Cast { annotation, .. } => *annotation,
      CastExport::Transcript { .. } | CastExport::Note { .. } => None,
    }
  }
}

/// Export-only CSS on top of the live stylesheet: the details rows carry the live page's
/// classes, so only the few live-control gaps need filling.
const EXPORT_EXTRA_CSS: &str = r#"
  .snapshot-note { color: var(--text-muted); font-size: 0.85rem; margin: -8px 0 16px; }
  .snapshot-note + .snapshot-note { margin-top: -8px; }
  /* The live page's corner for the job buttons (its rule is live-only CSS). */
  .session-actions { position: absolute; top: 0.7rem; right: 0.85rem; z-index: 2; margin: 0; }
  .session-actions .job-diff { min-width: 10.5rem; box-sizing: border-box; height: 1.85rem; }
  /* A commits diff takes the whole page over, like the live page's navigation to it. */
  iframe.diff-page {
    position: fixed; inset: 0; z-index: 3000; width: 100%; height: 100%;
    border: 0; background: #fff;
  }
  /* The live chip is a link to the annotator's job; offline it is a frozen status. */
  .cast-toolbar span.annotation-link { border: 1px solid var(--border); padding: 0.15rem 0.55rem; }
"#;

/// Assemble the whole-job page from the session's metadata and the per-proc exports
/// (`exports[i]` belongs to `session.procs[i]` — board order). `job_diff` is the packed
/// end-to-end commits diff of the whole job, when the run produced one. `now` is the
/// export instant: lifecycle, duration, and the workflow-node states freeze at it. Pure
/// beyond that: all file I/O (casts, sidecars, diffs) happened in the caller.
pub(crate) fn session_export_page(
  session: &Session, exports: &[CastExport], job_diff: Option<&str>, now: u64,
) -> String {
  let id = esc(&session.id);
  // Parity with the live job page: the lede (kind · lifecycle · task count) and the full
  // meta (ended, duration) ride along, so the offline copy answers "did it succeed, and
  // how long did it take" without the daemon.
  // The record alone reads completed while detached annotators still run; the export
  // learned their states from the daemon, so it reports the job the way the daemon does.
  let annotations: std::collections::BTreeMap<usize, &'static str> = session
    .procs
    .iter()
    .zip(exports)
    .filter_map(|(proc, export)| export.annotation().map(|status| (proc.index, status)))
    .collect();
  let annotating = annotations.values().filter(|status| **status == "running").count();
  let lifecycle = match session.lifecycle_status(now) {
    SessionLifecycle::Completed if annotating > 0 => SessionLifecycle::FinalAnnotations,
    other => other,
  };
  let lede = session_lede_html(session, lifecycle);
  let when = format!("{} UTC", crate::runtime::format_utc_timestamp(session.started_at));
  let ended = session_ended_text(session, lifecycle);
  let duration = session.duration_secs(now).map(format_duration_secs).unwrap_or_else(|| "—".into());
  // The workflow DAG (with its start/finish terminals) and the fleet comparison tables
  // are server-rendered markup styled by the shared stylesheet, so the export embeds them
  // as-is. Shared viewport controls keep this frozen state explorable without live updates.
  let workflow = workflow_graph_html_for(session, now, lifecycle, &annotations);
  // The daemon waits for annotation before exporting; a snapshot taken with `?nowait=1`
  // says how many recordings it left unfinished, so absent summaries read as timing.
  let pending_note = if annotating == 0 {
    String::new()
  } else {
    format!(
      "<p class=\"snapshot-note\">{annotating} recording{plural} still being annotated when this snapshot was taken — download it again later for {their} summary and chapters.</p>\n",
      plural = if annotating == 1 { " was" } else { "s were" },
      their = if annotating == 1 { "its" } else { "their" },
    )
  };
  let job_diff = job_diff.filter(|html| !html.is_empty());
  let job_diff_btn = job_diff_btn_html(job_diff.is_some());
  let errors = super::report::report_section_html(session, ReportSection::Errors);
  let results = super::report::report_section_html(session, ReportSection::Results);
  let log = super::report::report_section_html(session, ReportSection::Log);
  let prepared_by = snapshot_version_html();
  let mut fleet_sections = fleet_sections_by_anchor(session);
  let mut sections = String::new();
  let mut data_entries: Vec<String> = Vec::new();
  let mut diff_entries: Vec<String> = job_diff.map(|html| format!("\"all\": {}", quote(html))).into_iter().collect();
  for (proc, export) in session.procs.iter().zip(exports) {
    if let Some(html) = export.diff_html().filter(|html| !html.is_empty()) {
      diff_entries.push(format!("\"{}\": {}", proc.index, quote(html)));
    }
    sections.push_str(&proc_section(session, proc, export));
    if let Some(fleets) = fleet_sections.remove(&proc.index) {
      sections.push_str(&fleets);
    }
    if let CastExport::Cast { ndjson, summary, chapters, .. } = export {
      let markers: Vec<String> = chapters.iter().map(|(t, title)| format!("[{t}, {}]", quote(title))).collect();
      data_entries.push(format!(
        "{{ \"proc\": {idx}, \"cast\": {cast}, \"summary\": {summary}, \"markers\": [{markers}] }}",
        idx = proc.index,
        cast = quote(ndjson),
        summary = summary.as_deref().map(quote).unwrap_or_else(|| "null".into()),
        markers = markers.join(", "),
      ));
    }
  }
  // `</` never appears in the inline script: JSON strings escape it as `<\/`, so a hostile
  // recording or diff (a literal `</script>`) cannot terminate the block.
  let data = format!("[{}]", data_entries.join(",\n")).replace("</", "<\\/");
  let diffs = format!("{{{}}}", diff_entries.join(",\n")).replace("</", "<\\/");
  format!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
{favicon}
<title>scsh job {id}</title>
<style>{css}{player_css}{extra_css}</style>
</head>
<body>
<main class="page-shell">
<p class="page-lede">{lede}</p>
<div class="chamfer card card--accent-left-purple">{job_diff_btn}
<dl class="session-meta">
<dt>Job</dt><dd><code>{id}</code></dd>
<dt>Started</dt><dd>{when}</dd>
<dt>Ended</dt><dd>{ended}</dd>
<dt>Duration</dt><dd>{duration}</dd>
<dt>Repo</dt><dd><code class="repo-path">{repo}</code></dd>
<dt>Branch</dt><dd><code>{branch}</code></dd>
</dl>
</div>
<p class="snapshot-note">Offline snapshot prepared by {prepared_by} — everything below plays without a network.</p>
{pending_note}{errors}{results}{workflow}{log}<div class="procs">
{sections}</div>
</main>
<script>{workflow_view_js}
initWorkflowGraphView(step => {{
  const target = document.getElementById('task-' + step);
  const det = target && target.closest('details.proc');
  if (!det) return;
  det.open = true;
  location.hash = 'task-' + encodeURIComponent(step);
  det.scrollIntoView({{ block: 'start' }});
  const summary = det.querySelector('summary');
  if (summary) summary.focus({{ preventScroll: true }});
}});
</script>
<script>{player_js}</script>
<script>
const CASTS = {data};
CASTS.forEach((c) => {{
  const box = document.querySelector('.cast[data-proc="' + c.proc + '"]');
  const mount = box && box.querySelector('.cast-player');
  if (!mount) return;
  // A snapshot re-saved from the browser ("Save as") carries the players it had mounted,
  // as inert markup. The player appends to its mount, so an uncleared mount stacked a dead
  // copy above the working one: keys went to the dead copy, and fullscreen showed a pane
  // laid out for the inline width.
  mount.replaceChildren();
  // Chapters (c.markers) are player chrome: the ☰ panel, the seek-bar ticks, [/] keys.
  box._player = BeeCastPlayer.create({{ data: c.cast }}, mount, {{
    fit: 'both', controls: true, idleTimeLimit: 2, markers: c.markers,
    accessibility: 'snapshot',
  }});
}});
// Opening a row hands its player the keyboard, exactly like the live page.
document.querySelectorAll('details.proc').forEach((det) => det.addEventListener('toggle', () => {{
  if (!det.open) return;
  const root = det.querySelector('.beecast-player');
  if (root) {{ try {{ root.focus({{ preventScroll: true }}); }} catch (_) {{}} }}
}}));
</script>
<script>
// The packed commits-diff pages, keyed `all` (the whole job) or by proc index. The live
// page navigates to `/diff/<id>/<key>`; here the page is in the file, so `#diff-<key>` is
// the navigation: the hash entry is what packdiff's Back button (`history.back()`) and
// the browser's Back pop, and a reload lands on the same diff.
const DIFFS = {diffs};
const JOB_TITLE = document.title;
function syncDiffPage() {{
  const key = (location.hash.match(/^#diff-(\w+)$/) || [])[1];
  const html = key !== undefined && Object.prototype.hasOwnProperty.call(DIFFS, key) ? DIFFS[key] : null;
  let frame = document.querySelector('iframe.diff-page');
  if (frame && (html === null || frame.dataset.diff !== key)) {{ frame.remove(); frame = null; }}
  document.documentElement.style.overflow = html === null ? '' : 'hidden';
  if (html === null) {{ document.title = JOB_TITLE; return; }}
  if (frame) return;
  frame = document.createElement('iframe');
  frame.className = 'diff-page';
  frame.dataset.diff = key;
  // NOT sandboxed: a sandboxed frame may not traverse its parent's history, which is
  // exactly what packdiff's Back does. The page is scsh's own packdiff output. A srcdoc
  // document resolves `#fragment` against the PARENT's URL, which breaks packdiff's
  // `history.replaceState('#…')`; the base keeps its fragments its own.
  frame.srcdoc = html.replace('<head>', '<head><base href="about:srcdoc">');
  frame.addEventListener('load', () => {{
    try {{ document.title = frame.contentDocument.title || JOB_TITLE; }} catch (_) {{}}
    try {{ frame.contentWindow.focus(); }} catch (_) {{}}
  }});
  // First in the body, so a re-saved copy parses the stale frame before this script.
  document.body.prepend(frame);
}}
// A "Save as" copy taken with a diff open carries the frame as inert markup.
document.querySelectorAll('iframe.diff-page').forEach((frame) => frame.remove());
// Opened straight on a diff (a reload, a shared `#diff-all` link): put the job under it,
// so Back has somewhere to return to.
if (/^#diff-\w+$/.test(location.hash) && history.length === 1) {{
  const hash = location.hash;
  history.replaceState(null, '', location.pathname + location.search);
  location.hash = hash;
}}
window.addEventListener('hashchange', syncDiffPage);
syncDiffPage();
</script>
</body>
</html>
"#,
    favicon = FAVICON_LINK,
    css = PAGE_CSS,
    player_css = super::PLAYER_CSS,
    player_js = super::PLAYER_JS,
    workflow_view_js = WORKFLOW_VIEW_JS,
    extra_css = EXPORT_EXTRA_CSS,
    prepared_by = prepared_by,
    pending_note = pending_note,
    job_diff_btn = job_diff_btn,
    diffs = diffs,
    errors = errors,
    results = results,
    log = log,
    lede = lede,
    ended = esc(&ended),
    duration = esc(&duration),
    workflow = workflow,
    branch = esc(&session.branch),
    repo = esc(&session.repo),
  )
}

/// The scsh that prepared the snapshot, linked to its crates.io page: whoever receives
/// the file learns which version made it and where to install that same tool. The git
/// stamp follows when the build carries one.
fn snapshot_version_html() -> String {
  let link = format!(
    "<a href=\"https://crates.io/crates/scsh\" rel=\"noopener\">scsh {}</a>",
    esc(crate::version::pkg_version())
  );
  let git = crate::version::git_stamp();
  if git.is_empty() {
    link
  } else {
    format!("{link} · <code>{}</code>", esc(&git))
  }
}

/// The whole job's end-to-end commits diff — every step's commits as one review page —
/// behind the same button, in the same corner of the meta card, as the live page's
/// `⇄ all commits`. Absent when the run packed none.
fn job_diff_btn_html(has_diff: bool) -> String {
  if has_diff {
    r##"<div class="session-actions"><a class="chamfer btn btn--purple btn--sm job-diff" href="#diff-all" title="Browse the entire end-to-end commits diff"><span>⇄ all commits</span></a></div>"##.into()
  } else {
    String::new()
  }
}

/// A step's commits diff: the live page's `⇄ commits diff` button, on the summary row.
fn proc_diff_btn_html(proc: &ProcRecord, has_diff: bool) -> String {
  if has_diff {
    format!(
      r##"<a class="chamfer btn btn--purple btn--sm proc-diff" href="#diff-{idx}" title="Browse the commits this step brought into your branch — one self-contained review page"><span>⇄ commits diff</span></a>"##,
      idx = proc.index,
    )
  } else {
    String::new()
  }
}

/// One per-run row: the SAME `details.proc` markup as the live job page (triangle,
/// label, elapsed phrase, note, task anchor for the workflow graph's jump links), with
/// the cast box carrying only the keys hint — no live controls.
fn proc_section(session: &Session, proc: &ProcRecord, export: &CastExport) -> String {
  let note = proc_note_html(proc);
  let elapsed = proc_elapsed_phrase(proc, now_unix_secs());
  let has_diff = export.diff_html().is_some_and(|html| !html.is_empty());
  let body = match export {
    CastExport::Cast { summary, chapters, annotation, .. } => {
      let summary_html = match summary.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => format!("<div class=\"cast-summary\">{}</div>\n", esc(s)),
        None => String::new(),
      };
      let chapter_keys = if chapters.is_empty() { "" } else { " · [/] chapter · c chapters" };
      // The live page's chip links to the annotator's job; that page is not in the file,
      // so the snapshot keeps the status and drops the link.
      let annotation_chip = match annotation {
        Some(status) => {
          format!("<span class=\"annotation-link annotation-link--{status}\">{}</span>", annotation_label(status))
        }
        None => String::new(),
      };
      format!(
        "<div class=\"cast\" data-proc=\"{idx}\">\n{summary_html}<div class=\"cast-toolbar\">\
<span class=\"cast-keys dim\">space · ←/→ seek · &lt;/&gt; speed{chapter_keys} · f fullscreen</span>{annotation_chip}</div>\n\
<div class=\"cast-player\"></div>\n</div>\n",
        idx = proc.index,
        chapter_keys = chapter_keys,
        annotation_chip = annotation_chip,
      )
    }
    CastExport::Transcript { html, .. } => {
      format!("<div class=\"transcript\" data-proc=\"{}\">{html}</div>\n", proc.index)
    }
    // Parity with the live page: no text-log body exists anywhere — the cast is the
    // output format — so an unrecorded proc exports as its note row alone.
    CastExport::Note { text, .. } => {
      format!("<div class=\"detail dim\">{}</div>\n", esc(text))
    }
  };
  format!(
    r#"<details open class="chamfer proc {status}" id="proc-{idx}" data-index="{idx}"{task_attrs}>
<summary>
{task_anchor}
<span class="triangle" aria-hidden="true"></span>
<span class="label">{label}</span>{attempt_chip}
<span class="meta">{elapsed}</span>{retry_link}{original_link}
<span class="note dim">{note}</span>
{diff_chip}</summary>
{meta}
<div class="detail">{detail}</div>
{body}{usage}{quota}</details>
"#,
    status = proc.status.as_str(),
    idx = proc.index,
    task_attrs = proc_task_attrs(session, proc),
    task_anchor = proc_task_anchor_html(session, proc),
    label = esc(&proc.label),
    attempt_chip = super::session::attempt_chip_html(session, proc),
    retry_link = super::session::retry_link_html(session, proc),
    original_link = super::session::original_attempt_link_html(session, proc),
    elapsed = esc(&elapsed),
    note = note,
    detail = esc(proc.detail.as_deref().unwrap_or("")),
    diff_chip = proc_diff_btn_html(proc, has_diff),
    meta = proc_meta_html(proc),
    usage = proc_usage_html(proc),
    quota = proc_quota_html(proc),
  )
}
