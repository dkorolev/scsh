//! What a task may say about the job as a whole: markdown for the job page's errors,
//! results, and log sections.
//!
//! Two channels, one contract. Any task's result JSON — a flat `.scsh.yml` skill's, a
//! workflow step's, a host command's — may carry `errors_markdown`, `results_markdown`, and
//! `log_markdown` (strings; `error` is accepted for the errors section too). A host command
//! may instead append to the files named by `$SCSH_ERRORS_MD`, `$SCSH_RESULTS_MD`, and
//! `$SCSH_LOG_MD`, the natural shape for a shell script. Either way the text is appended to
//! the section under the task's name, is never forwarded to other steps, and never counts
//! against a workflow step's declared outputs.

use std::path::{Path, PathBuf};

use crate::daemon::{Client, ReportSection};
use crate::json::{self, Value};

/// The result-JSON keys a task may use, and the section each lands in. `error` is the
/// plain-string form an inner job is likely to write on its own.
pub const RESULT_KEYS: [(&str, ReportSection); 4] = [
  ("errors_markdown", ReportSection::Errors),
  ("error", ReportSection::Errors),
  ("results_markdown", ReportSection::Results),
  ("log_markdown", ReportSection::Log),
];

/// The environment variables a host command gets, and the section each file feeds.
pub const HOST_FILE_VARS: [(&str, ReportSection); 3] = [
  ("SCSH_ERRORS_MD", ReportSection::Errors),
  ("SCSH_RESULTS_MD", ReportSection::Results),
  ("SCSH_LOG_MD", ReportSection::Log),
];

/// Whether a result-JSON key is one of the job-page keys, which a workflow step may write
/// without declaring it.
pub fn is_result_key(name: &str) -> bool {
  RESULT_KEYS.iter().any(|(key, _)| *key == name)
}

/// The job-page contributions in a task's result JSON, in key order. Non-string values and
/// blank strings are ignored; unparseable text contributes nothing.
pub fn contributions_in_result(content: &str) -> Vec<(ReportSection, String)> {
  let Ok(Value::Object(fields)) = json::parse(content) else { return Vec::new() };
  RESULT_KEYS
    .iter()
    .filter_map(|(key, section)| match fields.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
      Some(Value::String(text)) if !text.trim().is_empty() => Some((*section, text.clone())),
      _ => None,
    })
    .collect()
}

/// Where a host command's three append files live: beside its result file, named after it.
pub fn host_files(result_path: &Path) -> Vec<(&'static str, ReportSection, PathBuf)> {
  HOST_FILE_VARS
    .iter()
    .map(|(var, section)| (*var, *section, result_path.with_extension(format!("{}.md", section.as_str()))))
    .collect()
}

/// Start a host command from empty files, so nothing an earlier attempt wrote is re-read.
pub fn clear_host_files(result_path: &Path) {
  for (_, _, path) in host_files(result_path) {
    let _ = std::fs::remove_file(path);
  }
}

/// What a host command appended to its files, in section order; missing or blank files
/// contribute nothing.
pub fn contributions_in_host_files(result_path: &Path) -> Vec<(ReportSection, String)> {
  host_files(result_path)
    .into_iter()
    .filter_map(|(_, section, path)| {
      let text = std::fs::read_to_string(path).ok()?;
      (!text.trim().is_empty()).then_some((section, text))
    })
    .collect()
}

/// Hand a task's contributions to the session browser, under the task's name. Nothing to
/// hand over, or no daemon, is fine.
pub fn publish(client: Option<&Client>, proc_index: usize, source: &str, contributions: &[(ReportSection, String)]) {
  let Some(client) = client else { return };
  for (section, markdown) in contributions {
    client.session_report(*section, Some(proc_index), source, markdown);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn result_keys_feed_their_sections_and_everything_else_is_ignored() {
    let content = r###"{ "grade": "good", "results_markdown": "## Totals\n\n- 3 routes", "error": "  ", "log_markdown": 7, "errors_markdown": "**boom**" }"###;
    let got = contributions_in_result(content);
    assert_eq!(
      got,
      vec![
        (ReportSection::Errors, "**boom**".to_string()),
        (ReportSection::Results, "## Totals\n\n- 3 routes".to_string())
      ]
    );
    assert!(contributions_in_result("not json").is_empty());
    assert!(contributions_in_result(r#"["results_markdown"]"#).is_empty());
    assert!(is_result_key("error") && is_result_key("log_markdown") && !is_result_key("grade"));
  }

  #[test]
  fn host_files_sit_beside_the_result_and_read_back_in_section_order() {
    let dir = std::env::temp_dir().join(format!("scsh-report-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let result = dir.join("publish.json");
    let files = host_files(&result);
    assert_eq!(files[0].0, "SCSH_ERRORS_MD");
    assert_eq!(files[1].2, dir.join("publish.results.md"));
    std::fs::write(&files[2].2, "log line\n").unwrap();
    std::fs::write(&files[1].2, "\n  \n").unwrap(); // blank: contributes nothing
    assert_eq!(contributions_in_host_files(&result), vec![(ReportSection::Log, "log line\n".to_string())]);
    clear_host_files(&result);
    assert!(contributions_in_host_files(&result).is_empty());
    std::fs::remove_dir_all(&dir).ok();
  }
}
