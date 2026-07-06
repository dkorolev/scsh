//! Session export: EVERY recording of a session assembled into ONE self-contained
//! offline `.html` page, served at `/session/<id>/export.html` as a download attachment.
//!
//! Composition is deliberate: each proc's recording is the EXACT per-cast export page
//! ([`crate::export::render_page_from_texts`] — the same bytes `/cast/…/export.html`
//! serves), embedded as an `<iframe srcdoc="…">` whose value is HTML-attribute-escaped
//! with [`super::escape::esc`]. That keeps the per-cast pipeline (beecast-page template,
//! vendored player, strict script-safe escaping) the single source of truth for how a
//! recording renders, and makes the session page pure assembly: a header, a per-proc
//! summary table, and one section per proc in board order.
//!
//! Honest tradeoff: each iframe carries its own copy of the vendored asciinema player
//! (~300KB), so an N-cast session page weighs roughly N player copies. A shared-bundle
//! multi-player page (one player, many casts) would need a beecast-page API extension —
//! noted as future work, not attempted here.

use super::escape::esc;
use super::format::format_elapsed_clock;
use super::layout::FAVICON_LINK;
use super::proc::status_glyph;
use crate::daemon::model::{ProcRecord, Session};

/// What the export gathered for one proc (aligned 1:1 with `session.procs`):
/// a rendered per-cast page (plus the sidecar's one-sentence summary, when annotated),
/// or a note explaining why there is nothing to embed — never an error.
pub(crate) enum CastExport {
  /// The full self-contained per-cast player page, to embed as an iframe `srcdoc`.
  Page { page: String, summary: Option<String> },
  /// No cast / no frames: a styled note row instead of an iframe.
  Note(String),
}

/// The inline CSS shell: same dark aesthetic as the daemon's pages, but fully
/// self-contained — an exported file must render offline with NO external requests.
const EXPORT_CSS: &str = r#"
  :root { color-scheme: dark; }
  body { margin: 0; background: #121317; color: #d7d9df; font: 14px/1.5 system-ui, sans-serif; }
  header { padding: 16px 20px 4px; }
  h1 { font-size: 1.25rem; font-weight: 600; margin: 0 0 0.25rem; }
  h2 { font-size: 1.05rem; font-weight: 600; margin: 0 0 0.25rem; }
  code { background: #1d1f26; padding: 1px 5px; border-radius: 4px; font-size: 0.85em; }
  .dim { color: #8a8d97; }
  .meta { display: flex; gap: 0.5rem 1.25rem; flex-wrap: wrap; font-size: 0.9rem; margin: 0.25rem 0 0.75rem; }
  .meta strong { font-weight: 600; margin-right: 0.3rem; }
  table { border-collapse: collapse; font-size: 0.9rem; margin: 0 20px 1rem; }
  thead tr, tbody tr { border-bottom: 1px solid #2a2d36; }
  th, td { text-align: left; padding: 0.3rem 0.75rem 0.3rem 0; vertical-align: top; }
  .glyph { font-weight: 600; }
  .ok .glyph { color: #3a8; }
  .fail .glyph { color: #e55; }
  details.proc { margin: 0 20px 1.5rem; border: 1px solid #2a2d36; border-radius: 6px; overflow: hidden; }
  details.proc > .proc-head { padding: 0.5rem 0.75rem; background: #1d1f26; display: flex; gap: 0.75rem; align-items: baseline; flex-wrap: wrap; cursor: pointer; }
  details.proc > .proc-head::marker { color: #8a8d97; }
  details.proc[open] > .proc-head { border-bottom: 1px solid #2a2d36; }
  .proc-summary { padding: 0.4rem 0.75rem; font-size: 0.9rem; background: #1a1c22; border-bottom: 1px solid #2a2d36; }
  .proc-note { padding: 1rem 0.75rem; color: #8a8d97; border-top: 1px dashed #2a2d36; }
  iframe.cast-page { display: block; width: 100%; height: 560px; border: 0; background: #000; }
"#;

/// Assemble the whole-session page from the session's metadata and the per-proc exports
/// gathered by the endpoint (`exports[i]` belongs to `session.procs[i]` — board order).
/// Pure: all file I/O (casts, sidecars) happened in the caller.
pub(crate) fn session_export_page(session: &Session, exports: &[CastExport]) -> String {
  let id = esc(&session.id);
  let profile = esc(session.profile.as_deref().unwrap_or("default"));
  let repo = esc(&session.repo);
  let when = format!("{} UTC", crate::runtime::format_utc_timestamp(session.started_at));
  let mut rows = String::new();
  let mut sections = String::new();
  for (proc, export) in session.procs.iter().zip(exports) {
    rows.push_str(&summary_row(proc));
    sections.push_str(&proc_section(proc, export));
  }
  format!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
{favicon}
<title>scsh session {id}</title>
<style>{css}</style>
</head>
<body>
<header>
<h1>scsh session <code>{id}</code></h1>
<div class="meta">
<span><strong>repo</strong> <code>{repo}</code></span>
<span><strong>profile</strong> {profile}</span>
<span><strong>when</strong> {when}</span>
</div>
</header>
<table>
<thead><tr><th>Proc</th><th>Status</th><th>Duration</th></tr></thead>
<tbody>
{rows}</tbody>
</table>
{sections}</body>
</html>
"#,
    favicon = FAVICON_LINK,
    css = EXPORT_CSS,
  )
}

fn duration_label(proc: &ProcRecord) -> String {
  proc.elapsed.map(format_elapsed_clock).unwrap_or_else(|| "—".to_string())
}

fn summary_row(proc: &ProcRecord) -> String {
  format!(
    "<tr class=\"{status}\"><td>{label}</td><td><span class=\"glyph\">{glyph}</span></td><td>{duration}</td></tr>\n",
    status = proc.status.as_str(),
    label = esc(&proc.label),
    glyph = status_glyph(proc.status),
    duration = esc(&duration_label(proc)),
  )
}

/// One per-proc section: a native `<details>` block — collapsible with zero JavaScript, so
/// the page stays pure self-contained HTML. The `<summary>` is the labelled head (glyph,
/// label, duration), informative while collapsed; sections default to open. The `srcdoc`
/// value is the whole per-cast page passed through [`esc`] — its `&`/`<`/`>`/`"` escaping
/// is exactly what a double-quoted HTML attribute needs, so a hostile recording (`"`, `&`,
/// `<`, even a literal `</iframe>`) can neither terminate the attribute nor leak markup
/// into the outer page.
fn proc_section(proc: &ProcRecord, export: &CastExport) -> String {
  let head = format!(
    "<summary class=\"proc-head\"><span class=\"glyph\">{glyph}</span> <strong>{label}</strong> \
<span class=\"dim\">{duration}</span></summary>\n",
    glyph = status_glyph(proc.status),
    label = esc(&proc.label),
    duration = esc(&duration_label(proc)),
  );
  let body = match export {
    CastExport::Page { page, summary } => {
      let summary_html = match summary {
        Some(s) => format!("<div class=\"proc-summary\">{}</div>\n", esc(s)),
        None => String::new(),
      };
      format!("{summary_html}<iframe class=\"cast-page\" loading=\"lazy\" srcdoc=\"{}\"></iframe>\n", esc(page))
    }
    CastExport::Note(note) => format!("<div class=\"proc-note\">{}</div>\n", esc(note)),
  };
  format!("<details open class=\"proc {status}\">\n{head}{body}</details>\n", status = proc.status.as_str())
}
