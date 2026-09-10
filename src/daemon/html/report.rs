//! The job page's errors, results, and log sections — what the job's tasks chose to say
//! about the job as a whole, in markdown, rendered above (errors, results) and below (log)
//! the job graph. Every section's container is always on the page, hidden while empty, so
//! the live script can fill it in place as contributions land.

use super::escape::esc;
use super::markdown;
use crate::daemon::model::{ReportEntry, ReportSection, Session};

/// The accent stripe each section wears: red for errors, green for results, cyan for the log.
fn accent(section: ReportSection) -> &'static str {
  match section {
    ReportSection::Errors => "card--accent-left-red",
    ReportSection::Results => "card--accent-left-green",
    ReportSection::Log => "card--accent-left-cyan",
  }
}

/// One section's card. Hidden (but present, with a stable id) when nothing was contributed.
pub(crate) fn report_section_html(session: &Session, section: ReportSection) -> String {
  let entries = session.report_for(section);
  let hidden = if entries.is_empty() { " hidden" } else { "" };
  format!(
    "<div class=\"chamfer card {accent} job-report\" id=\"job-{key}\" data-job-report=\"{key}\"{hidden}>\
<h2 class=\"report-title\">{title}</h2>\
<div class=\"report-body\" data-sig=\"{sig}\">{body}</div></div>\n",
    accent = accent(section),
    key = section.as_str(),
    title = section.title(),
    sig = report_signature(&entries),
    body = report_entries_html(&entries),
  )
}

/// A cheap fingerprint of a section's contributions, so the live script replaces a
/// section's markup only when something new arrived (a user may be selecting its text).
pub(crate) fn report_signature(entries: &[&ReportEntry]) -> String {
  let bytes: usize = entries.iter().map(|e| e.markdown.len()).sum();
  format!("{}:{bytes}", entries.len())
}

/// The contributions of one section, each under the name of the task that wrote it — the
/// caption is shown only when more than one task contributed, so a section with a single
/// author reads as prose, not as a list of attributions. Mirrored by `reportEntriesHtml`
/// in the client script.
pub(crate) fn report_entries_html(entries: &[&ReportEntry]) -> String {
  let mut sources: Vec<&str> = entries.iter().map(|e| e.source.as_str()).collect();
  sources.sort_unstable();
  sources.dedup();
  let attributed = sources.len() > 1;
  entries
    .iter()
    .map(|e| {
      let proc = e.proc.map(|p| format!(" data-proc=\"{p}\"")).unwrap_or_default();
      let caption = if attributed && !e.source.is_empty() {
        format!("<p class=\"report-source dim\">{}</p>", esc(&e.source))
      } else {
        String::new()
      };
      format!("<section class=\"report-entry\"{proc}>{caption}{}</section>", markdown::to_html(&e.markdown))
    })
    .collect()
}
