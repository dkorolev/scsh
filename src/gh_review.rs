//! `scsh gh-review …` — the host-side steps of the built-in `gh-gorgeous-review` workflow.
//!
//! The workflow (`harness_defs/gh-gorgeous-review.yml`) reviews a GitHub pull request with
//! every harness this machine can actually run, then publishes one review. Three of its
//! steps run on the host, because they need the operator's credentials rather than a model:
//!
//! - `plan`: which harnesses run. Credentials first ([`crate::runtime::check_harness_host`]),
//!   then quota ([`crate::quota::fetch`]): a harness sits out when its long window (weekly,
//!   monthly, billing cycle) has under 10% left or its 5-hour window under 25%. Fewer than
//!   [`MIN_HARNESSES`] runnable harnesses fails the plan — and with it the job — up front.
//! - `publish`: post the review the in-container `prepare_review` step wrote, through `gh`,
//!   with the head-unchanged and duplicate checks in [`crate::daemon::github_publish`].
//! - `quota-after`: the closing quota snapshot, reported as deltas against the plan's.
//!
//! Every decision is data on the job page: the plan's per-harness verdicts are its outputs,
//! each gated-off reviewer shows the verdict that skipped it, and the publish step carries
//! the review URL.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Harness;
use crate::json::{self, quote, Value};
use crate::quota::{self, HarnessQuota};

/// A review needs a second opinion: one harness is a run, not a fleet.
pub const MIN_HARNESSES: usize = 2;
/// A 5-hour window this far used (under 25% left) sits the harness out.
pub const SESSION_WINDOW_MAX_USED: f64 = 75.0;
/// A weekly / monthly / billing-cycle window this far used (under 10% left) sits it out.
pub const LONG_WINDOW_MAX_USED: f64 = 90.0;
/// Who prepares the review when several harnesses ran — the first runnable one wins.
pub const PUBLISHER_PREFERENCE: [Harness; 4] = [Harness::Claude, Harness::Codex, Harness::Cursor, Harness::Grok];

pub const VERDICT_RUN: &str = "run";
pub const VERDICT_NO_CREDENTIALS: &str = "no_credentials";
pub const VERDICT_EXPIRED: &str = "expired";
pub const VERDICT_LOW_QUOTA: &str = "low_quota";

/// One harness's place in the fleet, and the one-line reason a reader sees on the job page.
#[derive(Debug, Clone, PartialEq)]
pub struct HarnessPlan {
  pub harness: Harness,
  pub verdict: &'static str,
  pub note: String,
}

impl HarnessPlan {
  pub fn runs(&self) -> bool {
    self.verdict == VERDICT_RUN
  }
}

/// Decide one harness from what the host knows: its credential check and, when credentials
/// are present and the provider has a usage endpoint, its live quota. A quota probe that
/// failed or is unsupported does not bench the harness — the review still runs, and the note
/// says the reading was unavailable.
pub fn decide(harness: Harness, credentials: Result<(), String>, quota: Option<&HarnessQuota>) -> HarnessPlan {
  let name = harness.as_str();
  if let Err(why) = credentials {
    let verdict = if why.contains("expired") { VERDICT_EXPIRED } else { VERDICT_NO_CREDENTIALS };
    return HarnessPlan { harness, verdict, note: why };
  }
  let Some(q) = quota else {
    return HarnessPlan {
      harness,
      verdict: VERDICT_RUN,
      note: format!("{name}: credentials found; no quota endpoint"),
    };
  };
  match q.status {
    "missing" => HarnessPlan { harness, verdict: VERDICT_NO_CREDENTIALS, note: q.summary.clone() },
    "expired" => HarnessPlan { harness, verdict: VERDICT_EXPIRED, note: q.summary.clone() },
    "ok" => {
      for w in &q.windows {
        let (max_used, floor) = if w.id.starts_with("session") {
          (SESSION_WINDOW_MAX_USED, 100.0 - SESSION_WINDOW_MAX_USED)
        } else {
          (LONG_WINDOW_MAX_USED, 100.0 - LONG_WINDOW_MAX_USED)
        };
        if w.used_percent > max_used {
          return HarnessPlan {
            harness,
            verdict: VERDICT_LOW_QUOTA,
            note: format!("{name}: {} {:.0}% used, under the {floor:.0}% floor", w.label, w.used_percent),
          };
        }
      }
      HarnessPlan { harness, verdict: VERDICT_RUN, note: q.summary.clone() }
    }
    _ => HarnessPlan { harness, verdict: VERDICT_RUN, note: format!("{}; running anyway", q.summary) },
  }
}

/// The harness that prepares the review: the first runnable one in [`PUBLISHER_PREFERENCE`].
pub fn choose_publisher(plans: &[HarnessPlan]) -> Option<Harness> {
  PUBLISHER_PREFERENCE.into_iter().find(|h| plans.iter().any(|p| p.harness == *h && p.runs()))
}

pub fn runnable(plans: &[HarnessPlan]) -> Vec<Harness> {
  PUBLISHER_PREFERENCE.into_iter().filter(|h| plans.iter().any(|p| p.harness == *h && p.runs())).collect()
}

/// The plan step's `$SCSH_RESULT`: one enum verdict and one note per harness (the reviewer
/// gates read the verdict; the skip note quotes it), the publisher, the runnable list, and a
/// one-line summary.
pub fn plan_result_json(plans: &[HarnessPlan], publisher: Option<Harness>, quotas: &[HarnessQuota]) -> String {
  let mut fields = Vec::new();
  for p in plans {
    fields.push(format!("{}: {}", quote(p.harness.as_str()), quote(p.verdict)));
    fields.push(format!("{}: {}", quote(&format!("{}_note", p.harness.as_str())), quote(&p.note)));
  }
  let ran: Vec<&str> = runnable(plans).into_iter().map(|h| h.as_str()).collect();
  fields.push(format!("\"publisher\": {}", quote(publisher.map(|h| h.as_str()).unwrap_or("none"))));
  fields.push(format!("\"runnable\": {}", ran.len()));
  fields.push(format!("\"ran\": {}", quote(&ran.join(","))));
  fields.push(format!("\"summary\": {}", quote(&plan_summary(plans))));
  fields.push(format!("\"quota\": {}", quota_object_json(quotas)));
  format!("{{ {} }}", fields.join(", "))
}

/// The strongly-typed quota the plan hands downstream host steps: one entry per harness that
/// answered, each with its status and its percent-used windows (id, label, used, reset). A host
/// step reads this object instead of parsing the human note strings.
pub fn quota_object_json(quotas: &[HarnessQuota]) -> String {
  let mut entries = Vec::new();
  for q in quotas {
    let windows: Vec<String> = q
      .windows
      .iter()
      .map(|w| {
        format!(
          "{{ \"id\": {}, \"label\": {}, \"used_percent\": {}, \"resets_at\": {} }}",
          quote(&w.id),
          quote(&w.label),
          w.used_percent,
          w.resets_at.as_deref().map(quote).unwrap_or_else(|| "null".into()),
        )
      })
      .collect();
    let plan = q.plan.as_deref().map(quote).unwrap_or_else(|| "null".into());
    entries.push(format!(
      "{}: {{ \"status\": {}, \"plan\": {plan}, \"windows\": [{}] }}",
      quote(q.harness.as_str()),
      quote(q.status),
      windows.join(", "),
    ));
  }
  format!("{{ {} }}", entries.join(", "))
}

/// `claude ✓ · codex ✓ · cursor ✓ · grok ✗ expired`.
pub fn plan_summary(plans: &[HarnessPlan]) -> String {
  plans
    .iter()
    .map(|p| {
      if p.runs() {
        format!("{} ✓", p.harness.as_str())
      } else {
        format!("{} ✗ {}", p.harness.as_str(), p.verdict)
      }
    })
    .collect::<Vec<_>>()
    .join(" · ")
}

fn result_path() -> Result<PathBuf, String> {
  std::env::var_os("SCSH_RESULT").map(PathBuf::from).ok_or_else(|| {
    "SCSH_RESULT is not set — this command is a workflow host step, run it from the gh-gorgeous-review workflow".into()
  })
}

fn write_result(path: &Path, body: &str) -> Result<(), String> {
  if let Some(dir) = path.parent() {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
  }
  std::fs::write(path, body).map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn snapshot_path(phase: &str, harness: Harness) -> PathBuf {
  PathBuf::from("tmp").join(format!("quota-{phase}-{}.json", harness.as_str()))
}

/// Write the per-harness quota snapshot the agent-driven skill also keeps (`tmp/quota-<phase>-
/// <harness>.json`, in `scsh quota --json` shape), so a later `$gh-gorgeous-review` resume reads
/// the same files whether the browser or the terminal started the review.
fn write_snapshot(phase: &str, q: &HarnessQuota) {
  let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
  let path = snapshot_path(phase, q.harness);
  let _ = std::fs::create_dir_all("tmp");
  let _ = std::fs::write(&path, quota::render_json(std::slice::from_ref(q), now));
}

/// `scsh gh-review plan` — see the module docs. Exit 1 when fewer than [`MIN_HARNESSES`]
/// harnesses can run: the step's own result names every verdict, so the job page shows why.
pub fn plan_cmd() -> i32 {
  let result = match result_path() {
    Ok(p) => p,
    Err(e) => {
      eprintln!("{e}");
      return 1;
    }
  };
  let mut quotas: Vec<HarnessQuota> = Vec::new();
  let plans: Vec<HarnessPlan> = PUBLISHER_PREFERENCE
    .into_iter()
    .map(|h| {
      let credentials = crate::runtime::check_harness_host(h);
      let quota = if credentials.is_ok() && quota::SUPPORTED.contains(&h) { Some(quota::fetch(h)) } else { None };
      if let Some(q) = &quota {
        write_snapshot("before", q);
        quotas.push(q.clone());
      }
      decide(h, credentials, quota.as_ref())
    })
    .collect();
  for p in &plans {
    println!("{} {}: {}", if p.runs() { "✓" } else { "⊘" }, p.harness.as_str(), p.note);
  }
  let publisher = choose_publisher(&plans);
  let ran = runnable(&plans);
  let too_few = (ran.len() < MIN_HARNESSES).then(|| {
    format!(
      "gh-gorgeous-review needs at least {MIN_HARNESSES} runnable harnesses and this host has {}: {}",
      ran.len(),
      plan_summary(&plans)
    )
  });
  let mut body = plan_result_json(&plans, publisher, &quotas);
  // The job page: the fleet decision as the log, and a fleet too small as the error.
  let mut page = vec![format!("\"log_markdown\": {}", quote(&plan_log_markdown(&plans, publisher)))];
  if let Some(why) = &too_few {
    page.push(format!("\"errors_markdown\": {}", quote(why)));
  }
  body.insert_str(body.len() - 2, &format!(", {} ", page.join(", ")));
  if let Err(e) = write_result(&result, &body) {
    eprintln!("{e}");
    return 1;
  }
  if let Some(why) = too_few {
    eprintln!("{why}");
    return 1;
  }
  println!(
    "fleet: {} · publisher: {}",
    ran.iter().map(|h| h.as_str()).collect::<Vec<_>>().join(", "),
    publisher.map(|h| h.as_str()).unwrap_or("none")
  );
  0
}

fn env_string(name: &str) -> Result<String, String> {
  std::env::var(name).map_err(|_| format!("{name} is not set — the workflow binds it as a step input"))
}

fn git_rev(args: &[&str]) -> Result<String, String> {
  let out = Command::new("git").args(args).output().map_err(|e| format!("could not run git: {e}"))?;
  if !out.status.success() {
    return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
  }
  Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `scsh gh-review publish` — post the prepared review. Inputs (env, bound by the workflow):
/// `PR_URL`, `FINDINGS` (a JSON object whose `items` are reviewer issues: `file`, `line`,
/// `description`, `suggestion`), `SUMMARY` (the review's opening paragraph), `APPROVAL_BAR`
/// (`true` when every grade cleared the bar). The reviewed head is the replica's `HEAD^` —
/// the commit under the reconstructed-description notes commit — and the base is local `main`.
pub fn publish_cmd() -> i32 {
  match publish_inner() {
    Ok((url, event)) => {
      println!("published {event}: {url}");
      0
    }
    Err(e) => {
      eprintln!("publication failed: {e}");
      1
    }
  }
}

fn publish_inner() -> Result<(String, String), String> {
  let result = result_path()?;
  let url = env_string("PR_URL")?;
  let findings_text = env_string("FINDINGS")?;
  let summary = std::env::var("SUMMARY").unwrap_or_default();
  let approval_bar = std::env::var("APPROVAL_BAR").map(|v| v == "true").unwrap_or(false);
  let session = std::env::var("SCSH_SESSION").unwrap_or_default();
  let findings = match json::parse(&findings_text)? {
    Value::Object(fields) => match fields.into_iter().find(|(k, _)| k == "items").map(|(_, v)| v) {
      Some(Value::Array(items)) => items,
      _ => return Err("FINDINGS must be a JSON object with an `items` array".into()),
    },
    _ => return Err("FINDINGS must be a JSON object with an `items` array".into()),
  };
  let gh = crate::runtime::which("gh").ok_or("GitHub CLI is not installed; install gh and run 'gh auth login' once")?;
  let reference = crate::daemon::github::parse_pull_request(&url)?;
  let pr = crate::daemon::github::load_pull_request(&gh, reference)?;
  let root = std::env::current_dir().map_err(|e| e.to_string())?;
  let prepared = crate::daemon::github_publish::PreparedReview {
    head: git_rev(&["rev-parse", "HEAD^"])?,
    base: git_rev(&["rev-parse", "main"])?,
    approval_bar,
    findings,
    summary,
  };
  crate::daemon::github::write_browser_receipt(&root, &pr, &session, "publishing");
  let outcome = crate::daemon::github_publish::publish(&root, &pr, &session, &prepared);
  let state = if outcome.is_ok() { "published" } else { "publication_failed" };
  crate::daemon::github::write_browser_receipt(&root, &pr, &session, state);
  let (review_url, event) = outcome?;
  let results_markdown = publish_results_markdown(
    &event,
    &review_url,
    prepared.findings.len(),
    prepared.approval_bar,
    &reviewer_grades(&crate::runtime::session_results_dir(&session)),
  );
  write_result(
    &result,
    &format!(
      "{{ \"review_url\": {}, \"event\": {}, \"published\": true, \"results_markdown\": {} }}",
      quote(&review_url),
      quote(&event),
      quote(&results_markdown)
    ),
  )?;
  Ok((review_url, event))
}

/// One reviewer step's grade, read back from the job's durable results: the files are named
/// after the steps (`review_<skill>_<harness>.json`), so the name says who graded what.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewerGrade {
  pub skill: String,
  pub harness: String,
  pub grade: String,
}

/// Every reviewer grade the job persisted so far, in file order — the totals the published
/// review is summarized by. A results dir that does not exist yields no grades, not an error.
pub fn reviewer_grades(results_dir: &Path) -> Vec<ReviewerGrade> {
  let Ok(entries) = std::fs::read_dir(results_dir) else { return Vec::new() };
  let mut names: Vec<String> = entries
    .filter_map(|e| e.ok())
    .filter_map(|e| e.file_name().into_string().ok())
    .filter(|name| name.starts_with("review_") && name.ends_with(".json"))
    .collect();
  names.sort();
  names
    .into_iter()
    .filter_map(|name| {
      let stem = name.trim_end_matches(".json").trim_start_matches("review_");
      let (skill, harness) = stem.rsplit_once('_')?;
      let text = std::fs::read_to_string(results_dir.join(&name)).ok()?;
      let Value::Object(fields) = json::parse(&text).ok()? else { return None };
      let grade = fields.iter().find(|(k, _)| k == "grade").and_then(|(_, v)| match v {
        Value::String(s) => Some(s.clone()),
        _ => None,
      })?;
      Some(ReviewerGrade { skill: skill.replace('-', " "), harness: harness.to_string(), grade })
    })
    .collect()
}

/// The publish step's word on the job page: what was posted and where, and the fleet's
/// grades per reviewer with their mean — the totals a reader wants without opening rows.
pub fn publish_results_markdown(
  event: &str, review_url: &str, findings: usize, approval_bar: bool, grades: &[ReviewerGrade],
) -> String {
  let mut md = String::new();
  md.push_str("## Review published\n\n");
  md.push_str(&format!("- **{event}** — [open the review]({review_url})\n"));
  md.push_str(&format!(
    "- {findings} finding{} · approval bar {}\n",
    if findings == 1 { "" } else { "s" },
    if approval_bar { "cleared" } else { "not cleared" }
  ));
  if grades.is_empty() {
    return md;
  }
  let scores: Vec<u8> = grades.iter().filter_map(|g| crate::fleet::grade_score(&g.grade)).collect();
  let mean = if scores.is_empty() {
    String::new()
  } else {
    format!(
      " (mean {:.1} of 5 over {})",
      scores.iter().map(|s| f64::from(*s)).sum::<f64>() / scores.len() as f64,
      scores.len()
    )
  };
  md.push_str(&format!("\n## Grades{mean}\n\n"));
  let mut skills: Vec<&str> = grades.iter().map(|g| g.skill.as_str()).collect();
  skills.dedup();
  for skill in skills {
    let per: Vec<String> =
      grades.iter().filter(|g| g.skill == skill).map(|g| format!("{} {}", g.harness, g.grade)).collect();
    md.push_str(&format!("- **{skill}**: {}\n", per.join(" · ")));
  }
  md
}

/// The plan step's word on the job page: the fleet decision, one line per harness.
pub fn plan_log_markdown(plans: &[HarnessPlan], publisher: Option<Harness>) -> String {
  let mut md = String::from("## Fleet plan\n\n");
  for p in plans {
    md.push_str(&format!("- {} **{}** — {}\n", if p.runs() { "✓" } else { "⊘" }, p.harness.as_str(), p.note));
  }
  md.push_str(&format!("\nPublisher: **{}**\n", publisher.map(|h| h.as_str()).unwrap_or("none")));
  md
}

/// `scsh gh-review quota-after` — the closing snapshot for the harnesses that ran (`RAN`, a
/// comma-separated list from the plan), reported as per-window deltas against the plan's
/// snapshot. Best effort: a provider that will not answer is a line in the report, never a
/// failed step — the review is already published by the time this runs.
pub fn quota_after_cmd() -> i32 {
  let result = match result_path() {
    Ok(p) => p,
    Err(e) => {
      eprintln!("{e}");
      return 1;
    }
  };
  let ran = std::env::var("RAN").unwrap_or_default();
  let mut lines = Vec::new();
  let mut reports = Vec::new();
  for name in ran.split(',').map(str::trim).filter(|s| !s.is_empty()) {
    let Some(h) = Harness::parse(name) else { continue };
    if !quota::SUPPORTED.contains(&h) {
      lines.push(format!("{name}: no quota endpoint"));
      continue;
    }
    let after = quota::fetch(h);
    write_snapshot("after", &after);
    let report = quota_report(h, &after);
    lines.push(quota_delta_line(&report));
    reports.push(report);
  }
  for line in &lines {
    println!("{line}");
  }
  let body = format!(
    "{{ \"deltas\": {}, \"results_markdown\": {} }}",
    quote(&lines.join("\n")),
    quote(&quota_results_markdown(&reports))
  );
  if let Err(e) = write_result(&result, &body) {
    eprintln!("{e}");
    return 1;
  }
  0
}

/// One quota window before and after the review: what the review cost, and how many more
/// like it the window has room for.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowDelta {
  pub label: String,
  pub before: Option<f64>,
  pub after: f64,
  pub resets_at: Option<String>,
}

impl WindowDelta {
  pub fn delta(&self) -> Option<f64> {
    self.before.map(|b| self.after - b)
  }

  /// How many more reviews this window can take at the rate this one used, rounded down;
  /// `None` when the review cost nothing measurable (or nothing was measured), since a
  /// projection from zero is a guess dressed as a number.
  pub fn reviews_left(&self) -> Option<u64> {
    let delta = self.delta()?;
    if delta <= 0.0 || !delta.is_finite() {
      return None;
    }
    Some(((100.0 - self.after).max(0.0) / delta).floor() as u64)
  }
}

/// One harness's quota reading after the review, with its windows measured against the
/// plan's before-snapshot. A harness whose provider would not answer carries its summary
/// and no windows.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaReport {
  pub harness: String,
  pub plan: Option<String>,
  pub ok: bool,
  pub summary: String,
  pub windows: Vec<WindowDelta>,
}

fn quota_report(h: Harness, after: &HarnessQuota) -> QuotaReport {
  let before = std::fs::read_to_string(snapshot_path("before", h)).ok().and_then(|t| json::parse(&t).ok());
  let windows = if after.status == "ok" {
    after
      .windows
      .iter()
      .map(|w| WindowDelta {
        label: w.label.clone(),
        before: snapshot_used_percent(before.as_ref(), &w.id),
        after: w.used_percent,
        resets_at: w.resets_at.clone(),
      })
      .collect()
  } else {
    Vec::new()
  };
  QuotaReport {
    harness: h.as_str().to_string(),
    plan: after.plan.clone(),
    ok: after.status == "ok",
    summary: after.summary.clone(),
    windows,
  }
}

/// `claude: 5h session 3% → 9% (+6) · weekly 57% → 58% (+1)` — the `deltas` output line.
pub fn quota_delta_line(report: &QuotaReport) -> String {
  let name = &report.harness;
  if !report.ok {
    return format!("{name}: {}", report.summary);
  }
  let parts: Vec<String> = report
    .windows
    .iter()
    .map(|w| match w.before {
      Some(b) => format!("{} {b:.0}% → {:.0}% ({:+.0})", w.label, w.after, w.after - b),
      None => format!("{} {:.0}%", w.label, w.after),
    })
    .collect();
  format!("{name}: {}", parts.join(" · "))
}

/// The quota step's word on the job page: per harness, each window before → after with the
/// review's cost, when it resets, and how many more such reviews it has room for.
pub fn quota_results_markdown(reports: &[QuotaReport]) -> String {
  let mut md = String::from("## Quota after this review\n\n");
  if reports.is_empty() {
    md.push_str("No harness with a quota endpoint ran.\n");
    return md;
  }
  for r in reports {
    let plan = r.plan.as_deref().map(|p| format!(" ({p})")).unwrap_or_default();
    if !r.ok {
      md.push_str(&format!("- **{}**{plan}: {}\n", r.harness, r.summary));
      continue;
    }
    md.push_str(&format!("- **{}**{plan}\n", r.harness));
    for w in &r.windows {
      let cost = match w.delta() {
        Some(d) => format!("{:.0}% → {:.0}% ({})", w.before.unwrap_or(w.after), w.after, signed_percent(d)),
        None => format!("{:.0}% used", w.after),
      };
      let resets = w.resets_at.as_deref().map(|t| format!(", resets {t}")).unwrap_or_default();
      let room = match w.reviews_left() {
        Some(n) => format!(" — room for about {n} more review{} at this rate", if n == 1 { "" } else { "s" }),
        None if w.delta().is_some() => " — this review cost nothing measurable here".to_string(),
        None => String::new(),
      };
      md.push_str(&format!("  - {}: {cost}{resets}{room}\n", w.label));
    }
  }
  md
}

/// `+3`, `-1`, or `+0.2` — a delta under one point keeps a decimal, so a window that
/// projects room for more reviews never reads as having cost "+0".
fn signed_percent(d: f64) -> String {
  if d != 0.0 && d.abs() < 1.0 {
    format!("{d:+.1}")
  } else {
    format!("{d:+.0}")
  }
}

/// A window's `used_percent` from a `scsh quota --json` snapshot, by window id.
fn snapshot_used_percent(snapshot: Option<&Value>, id: &str) -> Option<f64> {
  let Value::Object(top) = snapshot? else { return None };
  let Value::Array(harnesses) = top.iter().find(|(k, _)| k == "harnesses").map(|(_, v)| v)? else { return None };
  let Value::Object(first) = harnesses.first()? else { return None };
  let Value::Array(windows) = first.iter().find(|(k, _)| k == "windows").map(|(_, v)| v)? else { return None };
  windows.iter().find_map(|w| {
    let Value::Object(fields) = w else { return None };
    let matches = fields.iter().any(|(k, v)| k == "id" && matches!(v, Value::String(s) if s == id));
    if !matches {
      return None;
    }
    fields
      .iter()
      .find(|(k, _)| k == "used_percent")
      .and_then(|(_, v)| if let Value::Number(n) = v { Some(*n) } else { None })
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_quota_window_projects_the_reviews_it_has_room_for() {
    let w =
      WindowDelta { label: "5h session".into(), before: Some(3.0), after: 9.0, resets_at: Some("18:00 UTC".into()) };
    assert_eq!(w.delta(), Some(6.0));
    assert_eq!(w.reviews_left(), Some(15), "91% left at 6% a review");
    let flat = WindowDelta { label: "weekly".into(), before: Some(57.0), after: 57.0, resets_at: None };
    assert_eq!(flat.reviews_left(), None, "a review that cost nothing projects nothing");
    let slight = WindowDelta { label: "pool".into(), before: Some(70.5), after: 70.7, resets_at: None };
    assert_eq!(slight.reviews_left(), Some(146));
    let md = quota_results_markdown(&[QuotaReport {
      harness: "cursor".into(),
      plan: None,
      ok: true,
      summary: String::new(),
      windows: vec![slight],
    }]);
    assert!(md.contains("  - pool: 70% → 71% (+0.2) — room for about 146 more reviews at this rate\n"), "got: {md}");
    let unknown = WindowDelta { label: "weekly".into(), before: None, after: 58.0, resets_at: None };
    assert_eq!(unknown.reviews_left(), None);
    let md = quota_results_markdown(&[QuotaReport {
      harness: "claude".into(),
      plan: Some("max".into()),
      ok: true,
      summary: String::new(),
      windows: vec![w, flat],
    }]);
    assert!(md.contains("- **claude** (max)\n"), "got: {md}");
    assert!(
      md.contains("  - 5h session: 3% → 9% (+6), resets 18:00 UTC — room for about 15 more reviews at this rate\n"),
      "got: {md}"
    );
    assert!(md.contains("  - weekly: 57% → 57% (+0) — this review cost nothing measurable here\n"), "got: {md}");
    let down = quota_results_markdown(&[QuotaReport {
      harness: "codex".into(),
      plan: None,
      ok: false,
      summary: "codex: the endpoint throttles bursts".into(),
      windows: Vec::new(),
    }]);
    assert!(down.contains("- **codex**: codex: the endpoint throttles bursts\n"), "got: {down}");
    assert!(quota_results_markdown(&[]).contains("No harness with a quota endpoint ran."));
  }

  #[test]
  fn the_published_review_is_summarized_with_every_reviewer_grade_and_their_mean() {
    let dir = std::env::temp_dir().join(format!("scsh-gh-grades-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("review_sanity-reviewer_claude.json"), r#"{ "grade": "excellent", "comments": [] }"#)
      .unwrap();
    std::fs::write(dir.join("review_sanity-reviewer_codex.json"), r#"{ "grade": "good", "comments": ["x"] }"#).unwrap();
    std::fs::write(dir.join("review_testing-reviewer_claude.json"), r#"{ "grade": "poor", "comments": [] }"#).unwrap();
    std::fs::write(dir.join("plan.json"), r#"{ "grade": "excellent" }"#).unwrap(); // not a reviewer
    std::fs::write(dir.join("review_broken_cursor.json"), "not json").unwrap();
    let grades = reviewer_grades(&dir);
    assert_eq!(
      grades,
      vec![
        ReviewerGrade { skill: "sanity reviewer".into(), harness: "claude".into(), grade: "excellent".into() },
        ReviewerGrade { skill: "sanity reviewer".into(), harness: "codex".into(), grade: "good".into() },
        ReviewerGrade { skill: "testing reviewer".into(), harness: "claude".into(), grade: "poor".into() },
      ]
    );
    let md =
      publish_results_markdown("COMMENT", "https://github.com/o/r/pull/7#pullrequestreview-1", 2, false, &grades);
    assert!(md.starts_with("## Review published\n\n- **COMMENT** — [open the review](https://github.com/o/r/pull/7#pullrequestreview-1)\n- 2 findings · approval bar not cleared\n"), "got: {md}");
    assert!(md.contains("## Grades (mean 3.7 of 5 over 3)\n"), "got: {md}");
    assert!(md.contains("- **sanity reviewer**: claude excellent · codex good\n"), "got: {md}");
    assert!(md.contains("- **testing reviewer**: claude poor\n"), "got: {md}");
    assert!(reviewer_grades(&dir.join("missing")).is_empty());
    std::fs::remove_dir_all(&dir).ok();
    let bare = publish_results_markdown("APPROVE", "u", 1, true, &[]);
    assert_eq!(
      bare,
      "## Review published\n\n- **APPROVE** — [open the review](u)\n- 1 finding · approval bar cleared\n"
    );
  }
  use crate::quota::QuotaWindow;

  fn quota(harness: Harness, status: &'static str, windows: Vec<(&str, f64)>) -> HarnessQuota {
    HarnessQuota {
      harness,
      status,
      plan: None,
      windows: windows
        .into_iter()
        .map(|(id, used)| QuotaWindow {
          id: id.into(),
          label: id.replace('_', " "),
          used_percent: used,
          resets_at: None,
        })
        .collect(),
      summary: format!("{} summary", harness.as_str()),
      hint: String::new(),
      source: "endpoint",
      observed_at: None,
    }
  }

  /// Credentials decide first, then each window against its own floor: 5-hour windows bench
  /// at 75% used, long windows at 90%; a probe that could not read benches nothing.
  #[test]
  fn plan_verdicts_follow_credentials_then_quota_floors() {
    let expired = decide(Harness::Grok, Err("grok login has expired — run `grok` on the host".into()), None);
    assert_eq!(expired.verdict, VERDICT_EXPIRED);
    let missing = decide(Harness::Cursor, Err("cursor harness unavailable (no cursor auth on host)".into()), None);
    assert_eq!(missing.verdict, VERDICT_NO_CREDENTIALS);
    let fine = decide(
      Harness::Claude,
      Ok(()),
      Some(&quota(Harness::Claude, "ok", vec![("session_5h", 74.9), ("weekly", 89.9)])),
    );
    assert_eq!(fine.verdict, VERDICT_RUN);
    let hot_session = decide(
      Harness::Claude,
      Ok(()),
      Some(&quota(Harness::Claude, "ok", vec![("session_5h", 75.1), ("weekly", 10.0)])),
    );
    assert_eq!(hot_session.verdict, VERDICT_LOW_QUOTA);
    assert!(hot_session.note.contains("session 5h 75% used, under the 25% floor"), "{}", hot_session.note);
    let dry_week =
      decide(Harness::Codex, Ok(()), Some(&quota(Harness::Codex, "ok", vec![("session_5h", 1.0), ("weekly", 90.5)])));
    assert_eq!(dry_week.verdict, VERDICT_LOW_QUOTA);
    let dry_cycle = decide(Harness::Cursor, Ok(()), Some(&quota(Harness::Cursor, "ok", vec![("billing_cycle", 95.0)])));
    assert_eq!(dry_cycle.verdict, VERDICT_LOW_QUOTA);
    let unreadable = decide(Harness::Codex, Ok(()), Some(&quota(Harness::Codex, "error", vec![])));
    assert_eq!(unreadable.verdict, VERDICT_RUN);
    assert!(unreadable.note.ends_with("running anyway"), "{}", unreadable.note);
    let lapsed = decide(Harness::Claude, Ok(()), Some(&quota(Harness::Claude, "expired", vec![])));
    assert_eq!(lapsed.verdict, VERDICT_EXPIRED);
  }

  /// The publisher is the first runnable harness in preference order; the result JSON carries
  /// every verdict as the enum the reviewer gates compare against, plus the runnable list.
  #[test]
  fn plan_result_names_the_publisher_and_every_verdict() {
    let plans = vec![
      decide(Harness::Claude, Err("claude harness unavailable".into()), None),
      decide(Harness::Codex, Ok(()), Some(&quota(Harness::Codex, "ok", vec![("weekly", 5.0)]))),
      decide(Harness::Cursor, Ok(()), Some(&quota(Harness::Cursor, "ok", vec![("billing_cycle", 5.0)]))),
      decide(Harness::Grok, Err("grok login has expired".into()), None),
    ];
    assert_eq!(choose_publisher(&plans), Some(Harness::Codex));
    assert_eq!(runnable(&plans), vec![Harness::Codex, Harness::Cursor]);
    let result = json::parse(&plan_result_json(&plans, choose_publisher(&plans), &[])).expect("valid json");
    let field = |k: &str| match &result {
      Value::Object(f) => f.iter().find(|(name, _)| name == k).map(|(_, v)| v.clone()),
      _ => None,
    };
    assert_eq!(field("claude"), Some(Value::String(VERDICT_NO_CREDENTIALS.into())));
    assert_eq!(field("grok"), Some(Value::String(VERDICT_EXPIRED.into())));
    assert_eq!(field("codex"), Some(Value::String(VERDICT_RUN.into())));
    assert_eq!(field("publisher"), Some(Value::String("codex".into())));
    assert_eq!(field("runnable"), Some(Value::Number(2.0)));
    assert_eq!(field("ran"), Some(Value::String("codex,cursor".into())));
    assert_eq!(plan_summary(&plans), "claude ✗ no_credentials · codex ✓ · cursor ✓ · grok ✗ expired");
    let solo = vec![decide(Harness::Codex, Ok(()), None), decide(Harness::Claude, Err("no".into()), None)];
    assert!(runnable(&solo).len() < MIN_HARNESSES, "one harness is not a fleet");
  }

  /// The plan hands downstream steps a strongly-typed `quota` object: per-harness status and
  /// percent-used windows, not a note string to parse.
  #[test]
  fn plan_quota_object_carries_typed_windows() {
    let quotas = vec![quota(Harness::Claude, "ok", vec![("session_5h", 41.0), ("weekly", 17.0)])];
    let obj = crate::json::parse(&quota_object_json(&quotas)).expect("valid json");
    let crate::json::Value::Object(top) = obj else { panic!("object") };
    let claude = top.iter().find(|(k, _)| k == "claude").map(|(_, v)| v).expect("claude entry");
    let crate::json::Value::Object(fields) = claude else { panic!("claude object") };
    assert!(
      matches!(fields.iter().find(|(k, _)| k == "status").map(|(_, v)| v), Some(crate::json::Value::String(s)) if s == "ok")
    );
    let crate::json::Value::Array(windows) = fields.iter().find(|(k, _)| k == "windows").map(|(_, v)| v).unwrap()
    else {
      panic!("windows array")
    };
    assert_eq!(windows.len(), 2);
    let crate::json::Value::Object(w0) = &windows[0] else { panic!("window object") };
    assert!(
      matches!(w0.iter().find(|(k, _)| k == "used_percent").map(|(_, v)| v), Some(crate::json::Value::Number(n)) if (*n - 41.0).abs() < 1e-9)
    );
  }
}
