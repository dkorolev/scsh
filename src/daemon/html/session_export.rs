//! Job export: EVERY recording of a job assembled into ONE self-contained offline `.html`
//! page, served at `/session/<id>/export.html` as a download attachment.
//!
//! The page is a REPLICA of the live job page: the same stylesheet (`layout::PAGE_CSS`),
//! the same purple island and collapsible per-run rows, and the same `beecast-player` —
//! one shared bundle, one player per recording, mounted from inline data (no iframes, no
//! per-cast page copies). What the live page does over HTTP the export inlines: the cast
//! text, the sidecar summary, and the chapter markers ride in a single JSON block, and a
//! small boot script mounts the players with the exact options the live page uses
//! (`fit: 'both'`, idle compression, chapter markers, `fullscreenEl` = the cast box,
//! focus-on-open). Live-only machinery — WebSocket, Live toggle, reload, downloads —
//! simply is not there.

use super::escape::esc;
use super::format::format_elapsed_clock;
use super::layout::{FAVICON_LINK, PAGE_CSS};
use super::proc::{proc_meta_html, status_glyph};
use crate::daemon::model::{ProcRecord, Session};
use crate::json::quote;

/// What the export gathered for one proc (aligned 1:1 with `session.procs`): the raw
/// recording plus its sidecar's summary and chapters, or a note explaining why there is
/// nothing to embed — never an error.
pub(crate) enum CastExport {
  Cast { ndjson: String, summary: Option<String>, chapters: Vec<(f64, String)> },
  Note(String),
}

/// Export-only CSS on top of the live stylesheet: the details rows carry the live page's
/// classes, so only the few live-control gaps need filling.
const EXPORT_EXTRA_CSS: &str = r#"
  .snapshot-note { color: var(--text-muted); font-size: 0.85rem; margin: -8px 0 16px; }
"#;

/// Assemble the whole-job page from the session's metadata and the per-proc exports
/// (`exports[i]` belongs to `session.procs[i]` — board order). Pure: all file I/O (casts,
/// sidecars) happened in the caller.
pub(crate) fn session_export_page(session: &Session, exports: &[CastExport]) -> String {
  let id = esc(&session.id);
  let profile = esc(session.profile.as_deref().unwrap_or("default"));
  let kind = esc(session.kind.as_deref().unwrap_or("profile"));
  let when = format!("{} UTC", crate::runtime::format_utc_timestamp(session.started_at));
  let mut sections = String::new();
  let mut data_entries: Vec<String> = Vec::new();
  for (proc, export) in session.procs.iter().zip(exports) {
    sections.push_str(&proc_section(proc, export));
    if let CastExport::Cast { ndjson, summary, chapters } = export {
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
<div class="card card--accent-left-purple">
<p class="session-kind">{kind} <strong>{profile}</strong></p>
<dl class="session-meta">
<dt>Job</dt><dd><code>{id}</code></dd>
<dt>Started</dt><dd>{when}</dd>
<dt>Branch</dt><dd><code>{branch}</code></dd>
<dt>Repo</dt><dd><code class="repo-path">{repo}</code></dd>
</dl>
</div>
<p class="snapshot-note">Offline snapshot — everything below plays without a network.</p>
<div class="procs">
{sections}</div>
<script>{player_js}</script>
<script>
const CASTS = {data};
CASTS.forEach((c) => {{
  const box = document.querySelector('.cast[data-proc="' + c.proc + '"]');
  const mount = box && box.querySelector('.cast-player');
  if (!mount) return;
  const player = BeeCastPlayer.create({{ data: c.cast }}, mount, {{
    fit: 'both', controls: true, idleTimeLimit: 2, markers: c.markers, fullscreenEl: box,
  }});
  box._player = player;
  box.querySelectorAll('.cast-chapters [data-seek]').forEach((btn) => btn.addEventListener('click', () => {{
    player.seek(Number(btn.dataset.seek));
    player.play();
  }}));
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
    branch = esc(&session.branch),
    repo = esc(&session.repo),
  )
}

fn duration_label(proc: &ProcRecord) -> String {
  proc.elapsed.map(format_elapsed_clock).unwrap_or_else(|| "—".to_string())
}

/// One per-run row: the SAME `details.proc` markup as the live job page (triangle, glyph,
/// label, elapsed, note), with the cast box carrying only the keys hint — no live controls.
fn proc_section(proc: &ProcRecord, export: &CastExport) -> String {
  let note = proc.detail.as_deref().or(proc.note.as_deref()).unwrap_or("");
  let body = match export {
    CastExport::Cast { summary, chapters, .. } => {
      let summary_html = match summary.as_deref().filter(|s| !s.is_empty()) {
        Some(s) => format!("<div class=\"cast-summary\">{}</div>\n", esc(s)),
        None => String::new(),
      };
      let chips: Vec<String> = chapters
        .iter()
        .map(|(t, title)| {
          format!(
            "<button type=\"button\" data-seek=\"{t}\">{clock} {title}</button>",
            clock = format_clock(*t),
            title = esc(title),
          )
        })
        .collect();
      let chapters_html = if chips.is_empty() {
        String::new()
      } else {
        format!("<div class=\"cast-chapters\">{}</div>\n", chips.join(""))
      };
      format!(
        "<div class=\"cast\" data-proc=\"{idx}\">\n{summary_html}<div class=\"cast-toolbar\">\
<span class=\"cast-keys dim\">space · ←/→ seek · &lt;/&gt; speed · [/] chapter · f fullscreen</span></div>\n\
{chapters_html}<div class=\"cast-player\"></div>\n</div>\n",
        idx = proc.index,
      )
    }
    CastExport::Note(text) => format!("<div class=\"detail dim\">{}</div>\n", esc(text)),
  };
  format!(
    r#"<details open class="proc {status}" data-index="{idx}">
<summary>
<span class="triangle" aria-hidden="true"></span><span class="glyph">{glyph}</span>
<span class="label">{label}</span>
<span class="meta">{elapsed}</span>
<span class="note dim">{note}</span>
</summary>
{meta}
{body}</details>
"#,
    status = proc.status.as_str(),
    idx = proc.index,
    glyph = status_glyph(proc.status),
    label = esc(&proc.label),
    elapsed = esc(&duration_label(proc)),
    note = esc(note),
    meta = proc_meta_html(proc),
  )
}

fn format_clock(t: f64) -> String {
  let secs = t.max(0.0).floor() as u64;
  format!("{}:{:02}", secs / 60, secs % 60)
}
