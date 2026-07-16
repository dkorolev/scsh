//! Job export: EVERY recording of a job assembled into ONE self-contained offline `.html`
//! page, served at `/job/<id>/export.html` as a download attachment.
//!
//! The page is a REPLICA of the live job page: the same stylesheet (`layout::PAGE_CSS`),
//! the same lede and full meta (ended, duration), the same workflow DAG and fleet
//! comparison sections (static, frozen at export time), the same purple island and
//! collapsible per-run rows — text-log procs keep their timestamped lines — and the same
//! `beecast-player` —
//! one shared bundle, one player per recording, mounted from inline data (no iframes, no
//! per-cast page copies). What the live page does over HTTP the export inlines: the cast
//! text, the sidecar summary, and the chapter markers ride in a single JSON block, and a
//! small boot script mounts the players with the exact options the live page uses
//! (`fit: 'both'`, idle compression, chapter markers, player-owned fullscreen,
//! focus-on-open). Live-only machinery — WebSocket, Live toggle, reload, downloads,
//! Force stop — simply is not there (and `LIVE_ONLY_CSS` is not inlined). Packed
//! commits-diff pages (when present) ride as sandboxed
//! `srcdoc` iframes (`allow-scripts allow-same-origin` so packdiff's in-page WASM comment
//! engine and localStorage work — packdiff 0.4.4 document-first review) so the snapshot
//! stays a single file.

use super::escape::esc;
use super::fleet::fleet_sections_by_anchor;
use super::format::format_duration_secs;
use super::layout::{FAVICON_LINK, PAGE_CSS};
use super::proc::{elapsed_phrase, proc_meta_html};
use super::session::{session_ended_text, session_lede_html};
use super::workflow::{proc_task_attrs, workflow_graph_html};
use crate::daemon::model::{ProcRecord, Session};
use crate::json::quote;

/// What the export gathered for one proc (aligned 1:1 with `session.procs`): the raw
/// recording plus its sidecar's summary and chapters, or a note explaining why there is
/// nothing to embed — never an error. Optional packed commits-diff HTML for offline
/// review (same file the live `⇄ commits diff` chip opens).
pub(crate) enum CastExport {
  Cast { ndjson: String, summary: Option<String>, chapters: Vec<(f64, String)>, diff_html: Option<String> },
  Note { text: String, diff_html: Option<String> },
}

impl CastExport {
  pub(crate) fn diff_html(&self) -> Option<&str> {
    match self {
      CastExport::Cast { diff_html, .. } | CastExport::Note { diff_html, .. } => diff_html.as_deref(),
    }
  }
}

/// Export-only CSS on top of the live stylesheet: the details rows carry the live page's
/// classes, so only the few live-control gaps need filling.
const EXPORT_EXTRA_CSS: &str = r#"
  .snapshot-note { color: var(--text-muted); font-size: 0.85rem; margin: -8px 0 16px; }
"#;

/// Assemble the whole-job page from the session's metadata and the per-proc exports
/// (`exports[i]` belongs to `session.procs[i]` — board order). `now` is the export
/// instant: lifecycle, duration, and the workflow-node states freeze at it. Pure beyond
/// that: all file I/O (casts, sidecars, diffs) happened in the caller.
pub(crate) fn session_export_page(session: &Session, exports: &[CastExport], now: u64) -> String {
  let id = esc(&session.id);
  // Parity with the live job page: the lede (kind · lifecycle · task count) and the full
  // meta (ended, duration) ride along, so the offline copy answers "did it succeed, and
  // how long did it take" without the daemon.
  let lifecycle = session.lifecycle_status(now);
  let lede = session_lede_html(session, lifecycle);
  let when = format!("{} UTC", crate::runtime::format_utc_timestamp(session.started_at));
  let ended = session_ended_text(session, lifecycle);
  let duration = session.duration_secs(now).map(format_duration_secs).unwrap_or_else(|| "—".into());
  // The workflow DAG (with its start/finish terminals) and the fleet comparison tables
  // are server-rendered markup styled by the shared stylesheet, so the export embeds them
  // as-is — the static state at export time, no live-update wiring.
  let workflow = workflow_graph_html(session, now);
  let mut fleet_sections = fleet_sections_by_anchor(session);
  let mut sections = String::new();
  let mut data_entries: Vec<String> = Vec::new();
  for (proc, export) in session.procs.iter().zip(exports) {
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
  // recording (a literal `</script>`) cannot terminate the block.
  let data = format!("[{}]", data_entries.join(",\n")).replace("</", "<\\/");
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
<div class="chamfer card card--accent-left-purple">
<dl class="session-meta">
<dt>Job</dt><dd><code>{id}</code></dd>
<dt>Started</dt><dd>{when}</dd>
<dt>Ended</dt><dd>{ended}</dd>
<dt>Duration</dt><dd>{duration}</dd>
<dt>Repo</dt><dd><code class="repo-path">{repo}</code></dd>
<dt>Branch</dt><dd><code>{branch}</code></dd>
</dl>
</div>
<p class="snapshot-note">Offline snapshot — everything below plays without a network.</p>
{workflow}<div class="procs">
{sections}</div>
</main>
<script>{player_js}</script>
<script>
const CASTS = {data};
CASTS.forEach((c) => {{
  const box = document.querySelector('.cast[data-proc="' + c.proc + '"]');
  const mount = box && box.querySelector('.cast-player');
  if (!mount) return;
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
</body>
</html>
"#,
    favicon = FAVICON_LINK,
    css = PAGE_CSS,
    player_css = super::PLAYER_CSS,
    player_js = super::PLAYER_JS,
    extra_css = EXPORT_EXTRA_CSS,
    lede = lede,
    ended = esc(&ended),
    duration = esc(&duration),
    workflow = workflow,
    branch = esc(&session.branch),
    repo = esc(&session.repo),
  )
}

/// Escape packed-diff HTML for an iframe `srcdoc="…"` attribute: quote/amp for the
/// attribute, and break `</` sequences the same way CASTS JSON does, so a hostile page
/// cannot close the attribute or confuse surrounding markup.
fn srcdoc_attr(html: &str) -> String {
  html.replace('&', "&amp;").replace('"', "&quot;").replace("</", "<\\/")
}

fn diff_embed_html(diff_html: Option<&str>) -> String {
  let Some(html) = diff_html.filter(|h| !h.is_empty()) else {
    return String::new();
  };
  format!(
    r#"<details class="chamfer proc-diff"><summary>⇄ commits diff</summary><iframe sandbox="allow-scripts allow-same-origin" srcdoc="{srcdoc}"></iframe></details>
"#,
    srcdoc = srcdoc_attr(html),
  )
}

fn diff_chip_html(has_diff: bool) -> String {
  if has_diff {
    r#"<span class="proc-diff" title="Commits diff embedded below">⇄ commits diff</span>"#.into()
  } else {
    String::new()
  }
}

/// One per-run row: the SAME `details.proc` markup as the live job page (triangle,
/// label, elapsed phrase, note, task anchor for the workflow graph's jump links), with
/// the cast box carrying only the keys hint — no live controls.
fn proc_section(session: &Session, proc: &ProcRecord, export: &CastExport) -> String {
  let note = proc.detail.as_deref().or(proc.note.as_deref()).unwrap_or("");
  let elapsed = elapsed_phrase(proc.status, proc.elapsed, proc.fail_reason.as_deref());
  let diff = export.diff_html();
  let body = match export {
    CastExport::Cast { summary, chapters, .. } => {
      let summary_html = match summary.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => format!("<div class=\"cast-summary\">{}</div>\n", esc(s)),
        None => String::new(),
      };
      let chapter_keys = if chapters.is_empty() { "" } else { " · [/] chapter · c chapters" };
      format!(
        "<div class=\"cast\" data-proc=\"{idx}\">\n{summary_html}<div class=\"cast-toolbar\">\
<span class=\"cast-keys dim\">space · ←/→ seek · &lt;/&gt; speed{chapter_keys} · f fullscreen</span></div>\n\
<div class=\"cast-player\"></div>\n</div>\n{diff}",
        idx = proc.index,
        chapter_keys = chapter_keys,
        diff = diff_embed_html(diff),
      )
    }
    // Parity with the live page: a proc that ran without a recording keeps its full
    // timestamped log lines offline (same static markup as the live text-log body; the
    // auto-scroll control is live-only). The no-recording note is dropped here because
    // the lines ARE the record; it stays only when there is truly no output to show.
    CastExport::Note { .. } if !proc.lines.is_empty() => {
      let mut lines_html = String::new();
      for line in &proc.lines {
        lines_html.push_str(&format!(
          "<div class=\"line\"><span class=\"at\">+{at:.1}s</span> {text}</div>\n",
          at = line.at,
          text = esc(&line.text)
        ));
      }
      format!("<div class=\"chamfer output\">{lines_html}</div>\n{}", diff_embed_html(diff))
    }
    CastExport::Note { text, .. } => {
      format!("<div class=\"detail dim\">{}</div>\n{}", esc(text), diff_embed_html(diff))
    }
  };
  format!(
    r#"<details open class="chamfer proc {status}" data-index="{idx}"{task_attrs}>
<summary>
<span class="triangle" aria-hidden="true"></span>
<span class="label">{label}</span>
<span class="meta">{elapsed}</span>
<span class="note dim">{note}</span>
{diff_chip}</summary>
{meta}
{body}</details>
"#,
    status = proc.status.as_str(),
    idx = proc.index,
    task_attrs = proc_task_attrs(session, proc),
    label = esc(&proc.label),
    elapsed = esc(&elapsed),
    note = esc(note),
    diff_chip = diff_chip_html(diff.is_some()),
    meta = proc_meta_html(proc),
  )
}
