//! Fleet aggregation: group matrix routes that share a `skill_source` and write a
//! deterministic rollup JSON under the session. Job-page HTML lives in `daemon::html`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::daemon::{ProcKind, ProcRecord, ProcStatus};
use crate::json;
use crate::runtime;

/// Matrix route name for a resolved invocation (`codex-terra`), or `None` for a direct skill.
pub fn route_name<'a>(skill_name: &'a str, skill_source: &str) -> Option<&'a str> {
  if skill_name == skill_source {
    return None;
  }
  skill_name.strip_prefix(skill_source).and_then(|rest| rest.strip_prefix('-')).filter(|r| !r.is_empty())
}

/// Copy a skill's result JSON into `$SCSH_HOME/sessions/<id>/results/<invocation>.json`,
/// canonicalized to pretty two-space-indented JSON with non-ASCII left readable — agents write
/// whatever their serializer produces (compact, \uXXXX-escaped), and this store is the
/// human-facing copy every later reader (job page, resume, cache replay) sees. A result that
/// does not parse is copied byte-for-byte instead — never lost to a formatting nicety.
pub fn persist_skill_result(session_id: &str, skill_name: &str, host_result: &Path) -> Option<String> {
  if !host_result.is_file() {
    return None;
  }
  let dir = runtime::session_results_dir(session_id);
  std::fs::create_dir_all(&dir).ok()?;
  let safe = skill_name.replace('/', "_");
  let dest = dir.join(format!("{safe}.json"));
  let canonical = std::fs::read_to_string(host_result)
    .ok()
    .and_then(|raw| crate::json::parse(&raw).ok().map(|v| crate::json::write_pretty(&v)));
  match canonical {
    Some(pretty) => std::fs::write(&dest, pretty).ok()?,
    None => {
      std::fs::copy(host_result, &dest).ok()?;
    }
  }
  Some(dest.to_string_lossy().into_owned())
}

/// One route row inside a fleet group.
#[derive(Debug, Clone)]
pub struct FleetRoute {
  pub proc_index: usize,
  pub route: String,
  pub harness: String,
  pub model: Option<String>,
  pub status: ProcStatus,
  pub elapsed: Option<f64>,
  pub detail: Option<String>,
  pub grade: Option<String>,
  pub comments_count: Option<u64>,
  pub issues_found: Option<u64>,
  pub result_message: Option<String>,
}

/// Routes that share a `skill_source` (len ≥ 2).
#[derive(Debug, Clone)]
pub struct FleetGroup {
  pub skill_source: String,
  pub routes: Vec<FleetRoute>,
  pub summary: String,
}

/// Group skill procs by `skill_source` when at least two share one.
pub fn fleet_groups(procs: &[ProcRecord]) -> Vec<FleetGroup> {
  let mut by_source: BTreeMap<String, Vec<&ProcRecord>> = BTreeMap::new();
  for p in procs {
    if p.kind != ProcKind::Skill {
      continue;
    }
    let Some(src) = p.skill_source.as_deref() else { continue };
    by_source.entry(src.to_string()).or_default().push(p);
  }
  let mut out = Vec::new();
  for (skill_source, group) in by_source {
    // A failed attempt that was retried registers a second proc with the same skill
    // name; the newest attempt is the route's authoritative outcome, so superseded
    // attempts stay out of the comparison (their route is represented by the retry).
    let group: Vec<&ProcRecord> = group
      .iter()
      .filter(|p| !group.iter().any(|later| later.index > p.index && later.skill_name == p.skill_name))
      .copied()
      .collect();
    if group.len() < 2 {
      continue;
    }
    let mut routes = Vec::new();
    for p in group {
      let parsed = p.result_path.as_deref().and_then(parse_result_summary);
      routes.push(FleetRoute {
        proc_index: p.index,
        route: p.route.clone().unwrap_or_else(|| p.skill_name.clone().unwrap_or_else(|| format!("p{}", p.index))),
        harness: p.harness.clone().unwrap_or_default(),
        model: p.model.clone(),
        status: p.status,
        elapsed: p.elapsed,
        detail: p.detail.clone(),
        grade: parsed.as_ref().and_then(|s| s.grade.clone()),
        comments_count: parsed.as_ref().and_then(|s| s.comments_count),
        issues_found: parsed.as_ref().and_then(|s| s.issues_found),
        result_message: parsed.as_ref().and_then(|s| s.message.clone()).or_else(|| p.detail.clone()),
      });
    }
    let summary = summarize_group(&skill_source, &routes);
    // Completed → Running → Waiting top to bottom (route name is the tiebreak).
    routes.sort_by(|a, b| {
      fleet_status_stack_rank(a.status).cmp(&fleet_status_stack_rank(b.status)).then_with(|| a.route.cmp(&b.route))
    });
    out.push(FleetGroup { skill_source, routes, summary });
  }
  out
}

fn fleet_status_stack_rank(status: ProcStatus) -> u8 {
  match status {
    ProcStatus::Ok => 0,
    ProcStatus::Graceful => 1,
    ProcStatus::Fail => 2,
    ProcStatus::Skipped => 3,
    ProcStatus::Running => 4,
    ProcStatus::Waiting => 5,
  }
}

/// One group's rollup JSON — the exact body of `<skill_source>-rollup.json`, also served
/// inside the fleet API payload so the files and the endpoint can never drift.
pub fn group_rollup_json(g: &FleetGroup) -> String {
  let mut routes_json = Vec::new();
  let mut messages = Vec::new();
  let mut grades = Vec::new();
  let mut issues_total: u64 = 0;
  let mut ok = 0usize;
  let mut fail = 0usize;
  for r in &g.routes {
    if matches!(r.status, ProcStatus::Ok | ProcStatus::Graceful) {
      ok += 1;
    } else if r.status == ProcStatus::Fail {
      fail += 1;
    }
    if let Some(gr) = &r.grade {
      grades.push(gr.clone());
    }
    if let Some(n) = r.issues_found {
      issues_total = issues_total.saturating_add(n);
    }
    if let Some(m) = r.result_message.as_deref().or(r.detail.as_deref()) {
      messages.push(m.to_string());
    }
    routes_json.push(format!(
      "{{ \"route\": {}, \"harness\": {}, \"model\": {}, \"status\": {}, \"detail\": {}, \"grade\": {}, \"issues_found\": {} }}",
      json::quote(&r.route),
      json::quote(&r.harness),
      opt_json_str(r.model.as_deref()),
      json::quote(r.status.as_str()),
      opt_json_str(r.detail.as_deref()),
      opt_json_str(r.grade.as_deref()),
      r.issues_found.map(|n| n.to_string()).unwrap_or_else(|| "null".into()),
    ));
  }
  let agree = !messages.is_empty() && messages.iter().all(|m| m == &messages[0]);
  format!(
    "{{\n  \"skill_source\": {},\n  \"ok\": {ok},\n  \"fail\": {fail},\n  \"agree\": {},\n  \"issues_total\": {issues_total},\n  \"grades\": [{}],\n  \"routes\": [\n    {}\n  ]\n}}\n",
    json::quote(&g.skill_source),
    if agree { "true" } else { "false" },
    grades.iter().map(|gr| json::quote(gr)).collect::<Vec<_>>().join(", "),
    routes_json.join(",\n    "),
  )
}

/// Write deterministic rollup JSON for every multi-route skill_source.
pub fn write_rollups(session_id: &str, procs: &[ProcRecord]) -> Vec<PathBuf> {
  let groups = fleet_groups(procs);
  let mut written = Vec::new();
  let dir = runtime::session_results_dir(session_id);
  let _ = std::fs::create_dir_all(&dir);
  for g in groups {
    let path = dir.join(format!("{}-rollup.json", g.skill_source));
    if std::fs::write(&path, group_rollup_json(&g)).is_ok() {
      written.push(path);
    }
  }
  written
}

/// The fleet API payload (`GET /api/v1/session/<id>/fleet`): every group's rollup plus
/// the job-level verdict, computed from live proc records so it serves mid-run — no
/// waiting for the end-of-run rollup files.
pub fn fleet_json(session_id: &str, procs: &[ProcRecord]) -> String {
  let groups = fleet_groups(procs);
  let verdict_json = match fleet_verdict(&groups) {
    None => "null".to_string(),
    Some(v) => {
      let grades = v
        .grades
        .iter()
        .map(|(grade, n)| format!("{{ \"grade\": {}, \"count\": {n} }}", json::quote(grade)))
        .collect::<Vec<_>>()
        .join(", ");
      format!(
        "{{ \"routes\": {}, \"ok\": {}, \"fail\": {}, \"pending\": {}, \"mean_score\": {}, \"findings_total\": {}, \"grades\": [{grades}] }}",
        v.routes,
        v.ok,
        v.fail,
        v.pending,
        v.mean_score.map(|m| m.to_string()).unwrap_or_else(|| "null".into()),
        v.findings_total,
      )
    }
  };
  let groups_json = groups.iter().map(|g| group_rollup_json(g).trim_end().to_string()).collect::<Vec<_>>().join(", ");
  let rounds_json = job_rounds(procs).iter().map(round_summary_json).collect::<Vec<_>>().join(", ");
  format!(
    "{{ \"session\": {}, \"verdict\": {verdict_json}, \"rounds\": [{rounds_json}], \"groups\": [{groups_json}] }}",
    json::quote(session_id),
  )
}

/// One cycle's entry in the fleet payload's `rounds` array.
fn round_summary_json(r: &RoundSummary) -> String {
  let counts = r
    .counts
    .iter()
    .map(|(grade, n)| format!("{{ \"grade\": {}, \"count\": {n} }}", json::quote(grade)))
    .collect::<Vec<_>>()
    .join(", ");
  format!(
    "{{ \"iteration\": {}, \"step\": {}, \"proc\": {}, \"mean\": {}, \"verdict\": {}, \"approved\": {}, \"counts\": [{counts}] }}",
    r.iteration,
    json::quote(&r.step),
    r.proc_index,
    r.mean.map(|m| m.to_string()).unwrap_or_else(|| "null".into()),
    opt_json_str(r.verdict.as_deref()),
    r.approved.map(|a| a.to_string()).unwrap_or_else(|| "null".into()),
  )
}

fn opt_json_str(s: Option<&str>) -> String {
  match s {
    Some(v) => json::quote(v),
    None => "null".into(),
  }
}

struct ResultSummary {
  message: Option<String>,
  grade: Option<String>,
  comments_count: Option<u64>,
  issues_found: Option<u64>,
}

fn parse_result_summary(path: &str) -> Option<ResultSummary> {
  let text = std::fs::read_to_string(path).ok()?;
  let message = json::message(&text);
  let json::Value::Object(fields) = json::parse(&text).ok()? else { return None };
  let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
  let grade = match field("grade") {
    Some(json::Value::String(value)) => Some(value.clone()),
    _ => None,
  };
  let comments_count = match field("comments") {
    Some(json::Value::Array(comments)) if comments.iter().all(|value| matches!(value, json::Value::String(_))) => {
      Some(comments.len() as u64)
    }
    // Old session artifacts remain readable after the workflow moves to structured comments.
    Some(json::Value::String(comments)) => {
      let paragraphs = comments.split("\n\n").filter(|part| !part.trim().is_empty()).count() as u64;
      Some(paragraphs.max(u64::from(!comments.trim().is_empty())))
    }
    _ => match field("comment_count") {
      Some(json::Value::Number(value)) if value.is_finite() && value.fract() == 0.0 && *value >= 0.0 => {
        Some(*value as u64)
      }
      _ => None,
    },
  };
  let issues_found = match field("issues_found") {
    Some(json::Value::Number(value)) if value.is_finite() && value.fract() == 0.0 && *value >= 0.0 => {
      Some(*value as u64)
    }
    _ => None,
  };

  // The code-review skills nest the summary one level down —
  // `{ "result": { "grade": ..., "issues_found": ... }, "issues": [...] }` — so when the
  // top level yields no grade/count, read the nested `result` object, and count the
  // `issues` array when `result.issues_found` is absent.
  let nested = match field("result") {
    Some(json::Value::Object(inner)) => inner.clone(),
    _ => Vec::new(),
  };
  let nested_field = |name: &str| nested.iter().find(|(key, _)| key == name).map(|(_, value)| value);
  let grade = grade.or_else(|| match nested_field("grade") {
    Some(json::Value::String(value)) => Some(value.clone()),
    _ => None,
  });
  let issues_found = issues_found
    .or_else(|| match nested_field("issues_found") {
      Some(json::Value::Number(value)) if value.is_finite() && value.fract() == 0.0 && *value >= 0.0 => {
        Some(*value as u64)
      }
      _ => None,
    })
    .or_else(|| match field("issues") {
      // Only the reviewer shape (a nested `result` object) counts its `issues` array.
      Some(json::Value::Array(issues)) if !nested.is_empty() => Some(issues.len() as u64),
      _ => None,
    });
  Some(ResultSummary { message, grade, comments_count, issues_found })
}

/// One loop cycle's score summary, read from the round-reporting step of that cycle.
///
/// Recognized by SHAPE, never by step name: any result carrying a numeric `mean` beside a
/// `counts` map of grade → count is a round report. A do-while pipeline that scores a review
/// fleet each cycle declares exactly that as one `object` output, so the report arrives one
/// level down under the author's own field name — which scsh never needs to know.
#[derive(Debug, Clone, PartialEq)]
pub struct RoundSummary {
  /// Loop iteration, 1-based, as parsed from the step's run id.
  pub iteration: usize,
  /// Base step id that reported the round (the run id without its loop suffix).
  pub step: String,
  pub proc_index: usize,
  /// Mean score the round reported; `None` when the report carried no usable number.
  pub mean: Option<f64>,
  /// Grade histogram for the round, highest score first (the [`fleet_verdict`] order).
  pub counts: Vec<(String, u64)>,
  /// The round's own bar verdict, when its step reported one alongside the numbers.
  pub verdict: Option<String>,
  pub approved: Option<bool>,
}

/// One round report parsed out of a result body — a [`RoundSummary`] before the cycle it
/// belongs to is known.
#[derive(Debug, Clone, PartialEq)]
struct RoundReport {
  mean: Option<f64>,
  counts: Vec<(String, u64)>,
  verdict: Option<String>,
  approved: Option<bool>,
}

/// The `{mean, counts}` pair inside one object, or `None` when it is not a round report. The
/// verdict fields stay empty here — they live at the result's top level, which the caller has.
fn read_round_report(obj: &[(String, json::Value)]) -> Option<RoundReport> {
  let field = |name: &str| obj.iter().find(|(key, _)| key == name).map(|(_, value)| value);
  // `counts` is what makes this a round report; a lone `mean` is too weak a signal to
  // claim an arbitrary result as one.
  let json::Value::Object(counts) = field("counts")? else { return None };
  let mut histogram: Vec<(String, u64)> = counts
    .iter()
    .filter_map(|(grade, value)| match value {
      json::Value::Number(n) if n.is_finite() && n.fract() == 0.0 && *n >= 0.0 => Some((grade.clone(), *n as u64)),
      _ => None,
    })
    .filter(|(_, n)| *n > 0)
    .collect();
  if histogram.is_empty() {
    return None;
  }
  histogram.sort_by(|a, b| {
    let rank = |g: &str| grade_score(g).map(|s| u8::MAX - s).unwrap_or(u8::MAX);
    rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0))
  });
  let mean = match field("mean") {
    Some(json::Value::Number(n)) if n.is_finite() => Some(*n),
    _ => None,
  };
  Some(RoundReport { mean, counts: histogram, verdict: None, approved: None })
}

/// Parse one round report out of a stored result body. The `{mean, counts}` pair is read at
/// the top level, or one level down under any object-valued field (where a declared `object`
/// output lands); `verdict` and `approved` always come from the top level, where a step's
/// scalar outputs live. See [`RoundSummary`] for the recognition rule.
fn parse_round_summary(text: &str) -> Option<RoundReport> {
  let json::Value::Object(fields) = json::parse(text).ok()? else { return None };
  let mut report = read_round_report(&fields).or_else(|| {
    fields.iter().find_map(|(_, value)| match value {
      json::Value::Object(inner) => read_round_report(inner),
      _ => None,
    })
  })?;
  let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
  report.verdict = match field("verdict") {
    Some(json::Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
    _ => None,
  };
  report.approved = match field("approved") {
    Some(json::Value::Bool(value)) => Some(*value),
    _ => None,
  };
  Some(report)
}

/// Every loop cycle that reported a score, oldest first — the job's convergence trajectory.
///
/// Empty for jobs whose loops report nothing scoreable, and for jobs with no loop at all.
pub fn job_rounds(procs: &[ProcRecord]) -> Vec<RoundSummary> {
  let mut rounds: Vec<RoundSummary> = Vec::new();
  for p in procs {
    if p.kind != ProcKind::Skill {
      continue;
    }
    let Some(name) = p.skill_name.as_deref() else { continue };
    let Some((base, _, iteration)) = crate::daemon::parse_loop_iteration_id(name) else { continue };
    let Some(path) = p.result_path.as_deref() else { continue };
    let Some(text) = std::fs::read_to_string(path).ok() else { continue };
    let Some(report) = parse_round_summary(&text) else { continue };
    let summary = RoundSummary {
      iteration,
      step: base.to_string(),
      proc_index: p.index,
      mean: report.mean,
      counts: report.counts,
      verdict: report.verdict,
      approved: report.approved,
    };
    // A retried cycle registers a second proc for the same iteration; the newest attempt is
    // that cycle's authoritative score, exactly as `fleet_groups` treats superseded routes.
    match rounds.iter_mut().find(|r| r.iteration == iteration && r.step == summary.step) {
      Some(existing) if existing.proc_index < summary.proc_index => *existing = summary,
      Some(_) => {}
      None => rounds.push(summary),
    }
  }
  rounds.sort_by_key(|r| r.iteration);
  rounds
}

/// Job-level roll-up across every fleet group: the whole-run view of a matrix review
/// fleet. Descriptive only — scsh reports the numbers (route counts, grade histogram,
/// mean score) and leaves any approval bar to the caller, consistent with scsh having
/// no built-in comparison language.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetVerdict {
  /// Total routes across all fleet groups.
  pub routes: usize,
  pub ok: usize,
  pub fail: usize,
  /// Routes not yet settled (running or waiting) — the verdict is partial while > 0.
  pub pending: usize,
  /// Grade histogram, highest score first (unrecognized grades trail alphabetically).
  pub grades: Vec<(String, usize)>,
  /// Mean over graded routes on the excellent=5 · good=4 · average=3 · poor=2 · bad=1
  /// scale; `None` until at least one route reports a recognized grade.
  pub mean_score: Option<f64>,
  /// Issues plus comments reported across all routes.
  pub findings_total: u64,
}

/// Score for the shared reviewer grade vocabulary; `None` for anything else.
pub fn grade_score(grade: &str) -> Option<u8> {
  match grade {
    "excellent" => Some(5),
    "good" => Some(4),
    "average" => Some(3),
    "poor" => Some(2),
    "bad" => Some(1),
    _ => None,
  }
}

/// Aggregate every group's routes into one [`FleetVerdict`]; `None` without fleet groups.
pub fn fleet_verdict(groups: &[FleetGroup]) -> Option<FleetVerdict> {
  if groups.is_empty() {
    return None;
  }
  let mut verdict =
    FleetVerdict { routes: 0, ok: 0, fail: 0, pending: 0, grades: Vec::new(), mean_score: None, findings_total: 0 };
  let mut histogram: BTreeMap<String, usize> = BTreeMap::new();
  let mut score_sum = 0u64;
  let mut score_n = 0u64;
  for r in groups.iter().flat_map(|g| g.routes.iter()) {
    verdict.routes += 1;
    match r.status {
      ProcStatus::Ok | ProcStatus::Graceful => verdict.ok += 1,
      ProcStatus::Fail => verdict.fail += 1,
      ProcStatus::Running | ProcStatus::Waiting => verdict.pending += 1,
      ProcStatus::Skipped => {}
    }
    if let Some(grade) = &r.grade {
      *histogram.entry(grade.clone()).or_default() += 1;
      if let Some(score) = grade_score(grade) {
        score_sum += u64::from(score);
        score_n += 1;
      }
    }
    let findings = r.issues_found.or(r.comments_count).unwrap_or(0);
    verdict.findings_total = verdict.findings_total.saturating_add(findings);
  }
  if score_n > 0 {
    verdict.mean_score = Some(score_sum as f64 / score_n as f64);
  }
  let mut grades: Vec<(String, usize)> = histogram.into_iter().collect();
  grades.sort_by(|a, b| {
    let rank = |g: &str| grade_score(g).map(|s| u8::MAX - s).unwrap_or(u8::MAX);
    rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0))
  });
  verdict.grades = grades;
  Some(verdict)
}

pub fn summarize_group(skill_source: &str, routes: &[FleetRoute]) -> String {
  let ok = routes.iter().filter(|r| matches!(r.status, ProcStatus::Ok | ProcStatus::Graceful)).count();
  let fail = routes.iter().filter(|r| r.status == ProcStatus::Fail).count();
  let msgs: Vec<_> = routes.iter().filter_map(|r| r.result_message.as_deref()).collect();
  let agree = !msgs.is_empty() && msgs.iter().all(|m| *m == msgs[0]);
  if agree {
    return format!("{skill_source}: {ok} ok, {fail} fail — all routes agree: {}", msgs[0]);
  }
  let issues: u64 = routes.iter().filter_map(|r| r.issues_found).sum();
  if issues > 0 || routes.iter().any(|r| r.grade.is_some()) {
    return format!("{skill_source}: {ok} ok, {fail} fail · {issues} issue(s) across routes");
  }
  format!("{skill_source}: {ok} ok, {fail} fail · {} routes", routes.len())
}

#[cfg(test)]
mod tests {
  #[test]
  fn persist_canonicalizes_parseable_results_and_copies_unparseable_ones_verbatim() {
    let home = std::env::temp_dir().join(format!("scsh-persist-{}", std::process::id()));
    let prev = std::env::var_os("SCSH_HOME");
    std::env::set_var("SCSH_HOME", &home);
    let src_dir = home.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let compact = src_dir.join("ok.json");
    std::fs::write(&compact, "{\"result\":{\"grade\":\"good\"},\"note\":\"\\u201chi\\u201d\"}").unwrap();
    let stored = persist_skill_result("prstst", "decide", &compact).unwrap();
    let body = std::fs::read_to_string(&stored).unwrap();
    assert!(body.contains("\n  \"result\": {"), "stored copy is pretty-printed: {body}");
    assert!(body.contains("\u{201c}hi\u{201d}"), "escape noise becomes readable text: {body}");

    let broken = src_dir.join("broken.json");
    std::fs::write(&broken, "not json at all").unwrap();
    let stored = persist_skill_result("prstst", "broken", &broken).unwrap();
    assert_eq!(std::fs::read_to_string(&stored).unwrap(), "not json at all", "unparseable results copy verbatim");

    let _ = std::fs::remove_dir_all(&home);
    match prev {
      Some(v) => std::env::set_var("SCSH_HOME", v),
      None => std::env::remove_var("SCSH_HOME"),
    }
  }

  use super::*;
  use crate::daemon::{ProcKind, ProcStatus};

  #[test]
  fn route_name_strips_skill_source_prefix() {
    assert_eq!(route_name("add-opencode", "add"), Some("opencode"));
    assert_eq!(route_name("add", "add"), None);
    assert_eq!(route_name("conventions-reviewer-codex-terra", "conventions-reviewer"), Some("codex-terra"));
  }

  #[test]
  fn result_summary_derives_structured_comment_count_and_reads_legacy_results() {
    let dir = std::env::temp_dir().join(format!("scsh-fleet-summary-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let structured = dir.join("structured.json");
    std::fs::write(&structured, r#"{"grade":"good","comments":["one","two"]}"#).unwrap();
    let summary = parse_result_summary(&structured.to_string_lossy()).unwrap();
    assert_eq!(summary.grade.as_deref(), Some("good"));
    assert_eq!(summary.comments_count, Some(2));

    let legacy = dir.join("legacy.json");
    std::fs::write(&legacy, r#"{"grade":"excellent","comments":"one legacy comment","comment_count":99}"#).unwrap();
    assert_eq!(parse_result_summary(&legacy.to_string_lossy()).unwrap().comments_count, Some(1));
    let _ = std::fs::remove_dir_all(dir);
  }

  #[test]
  fn result_summary_reads_the_code_review_skills_nested_shape() {
    let dir = std::env::temp_dir().join(format!("scsh-fleet-nested-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();

    // The exact shape dkorolev/code-review-skills documents for its reviewers:
    // `{ result: { grade, issues_found }, issues: Issue[] }` with issues_found == issues.length.
    let nested = dir.join("nested.json");
    std::fs::write(
      &nested,
      r#"{"result":{"grade":"excellent","issues_found":2},"issues":[
        {"commit":"abc1234","file":"src/a.rs","line":10,"description":"d","suggestion":"s"},
        {"commit":"abc1234","file":"PR-DESCRIPTION.md","line":0,"description":"d2","suggestion":"s2"}]}"#,
    )
    .unwrap();
    let summary = parse_result_summary(&nested.to_string_lossy()).unwrap();
    assert_eq!(summary.grade.as_deref(), Some("excellent"));
    assert_eq!(summary.issues_found, Some(2));
    assert_eq!(summary.comments_count, None);
    assert_eq!(summary.message.as_deref(), Some("grade: excellent · issues_found: 2"));

    // Without `result.issues_found`, the `issues` array length is the count.
    let counted = dir.join("counted.json");
    std::fs::write(
      &counted,
      r#"{"result":{"grade":"good"},"issues":[{"commit":"x","file":"f","line":1,"description":"d","suggestion":"s"}]}"#,
    )
    .unwrap();
    let summary = parse_result_summary(&counted.to_string_lossy()).unwrap();
    assert_eq!(summary.grade.as_deref(), Some("good"));
    assert_eq!(summary.issues_found, Some(1));

    // A top-level `issues` array alone (no nested `result` object) is NOT counted — only
    // the reviewer shape opts in.
    let plain = dir.join("plain.json");
    std::fs::write(&plain, r#"{"message":"done","issues":[1,2,3]}"#).unwrap();
    assert_eq!(parse_result_summary(&plain.to_string_lossy()).unwrap().issues_found, None);

    // The flat workflow-def shape still wins over the nested one when both exist.
    let flat = dir.join("flat.json");
    std::fs::write(&flat, r#"{"grade":"poor","result":{"grade":"excellent","issues_found":9}}"#).unwrap();
    assert_eq!(parse_result_summary(&flat.to_string_lossy()).unwrap().grade.as_deref(), Some("poor"));
    let _ = std::fs::remove_dir_all(dir);
  }

  fn verdict_route(status: ProcStatus, grade: Option<&str>, issues: Option<u64>) -> FleetRoute {
    FleetRoute {
      proc_index: 0,
      route: "r".into(),
      harness: "h".into(),
      model: None,
      status,
      elapsed: None,
      detail: None,
      grade: grade.map(str::to_string),
      comments_count: None,
      issues_found: issues,
      result_message: None,
    }
  }

  fn verdict_group(routes: Vec<FleetRoute>) -> FleetGroup {
    FleetGroup { skill_source: "review".into(), routes, summary: String::new() }
  }

  #[test]
  fn fleet_verdict_aggregates_grades_across_groups() {
    let groups = vec![
      verdict_group(vec![
        verdict_route(ProcStatus::Ok, Some("excellent"), Some(1)),
        verdict_route(ProcStatus::Ok, Some("good"), Some(2)),
      ]),
      verdict_group(vec![
        verdict_route(ProcStatus::Ok, Some("excellent"), None),
        verdict_route(ProcStatus::Fail, None, None),
        verdict_route(ProcStatus::Running, None, None),
      ]),
    ];
    let v = fleet_verdict(&groups).unwrap();
    assert_eq!((v.routes, v.ok, v.fail, v.pending), (5, 3, 1, 1));
    // Histogram is ordered highest score first.
    assert_eq!(v.grades, vec![("excellent".to_string(), 2), ("good".to_string(), 1)]);
    // (5 + 4 + 5) / 3
    assert!((v.mean_score.unwrap() - 14.0 / 3.0).abs() < 1e-9);
    assert_eq!(v.findings_total, 3);
  }

  #[test]
  fn fleet_verdict_handles_no_groups_and_unrecognized_grades() {
    assert_eq!(fleet_verdict(&[]), None);
    // An off-vocabulary grade shows in the histogram (after scored grades) but never
    // skews the mean.
    let groups = vec![verdict_group(vec![
      verdict_route(ProcStatus::Ok, Some("stellar"), None),
      verdict_route(ProcStatus::Ok, Some("good"), None),
    ])];
    let v = fleet_verdict(&groups).unwrap();
    assert_eq!(v.grades, vec![("good".to_string(), 1), ("stellar".to_string(), 1)]);
    assert!((v.mean_score.unwrap() - 4.0).abs() < 1e-9);
    // No recognized grade at all → no mean.
    let ungraded = vec![verdict_group(vec![verdict_route(ProcStatus::Ok, None, None)])];
    assert_eq!(fleet_verdict(&ungraded).unwrap().mean_score, None);
  }

  #[test]
  fn round_summary_is_recognized_by_shape_wherever_the_report_sits() {
    // The gorgeous-pipeline shape: the round report is one declared `object` output, so it
    // lands one level down under the author's own field name — which scsh never knows.
    let nested = r#"{"approved":false,"verdict":"not met","feedback":{
      "mean":4.27,"counts":{"excellent":7,"good":6,"average":2,"poor":0},
      "routes":{"conventions-opus":{"grade":"excellent","comments":[]}}}}"#;
    let report = parse_round_summary(nested).unwrap();
    assert_eq!(report.mean, Some(4.27));
    // Highest score first, and a zero-count grade never becomes a chip.
    assert_eq!(report.counts, vec![("excellent".into(), 7), ("good".into(), 6), ("average".into(), 2)]);
    assert_eq!(report.verdict.as_deref(), Some("not met"));
    assert_eq!(report.approved, Some(false));

    // The same numbers reported flat at the top level parse identically.
    let flat = r#"{"mean":4.5,"counts":{"good":2},"approved":true,"verdict":"met"}"#;
    let report = parse_round_summary(flat).unwrap();
    assert_eq!((report.mean, report.verdict.as_deref(), report.approved), (Some(4.5), Some("met"), Some(true)));
    assert_eq!(report.counts, vec![("good".into(), 2)]);

    // An off-vocabulary grade still shows, sorted after the known ones.
    let odd = r#"{"counts":{"stellar":1,"good":1}}"#;
    let report = parse_round_summary(odd).unwrap();
    assert_eq!(report.mean, None, "a report may carry counts without a mean");
    assert_eq!(report.counts, vec![("good".into(), 1), ("stellar".into(), 1)]);

    // Not round reports: no counts at all, counts that hold no usable numbers, and an
    // ordinary reviewer result. A lone `mean` must not claim an unrelated result.
    for text in [
      r#"{"mean":4.27,"verdict":"met"}"#,
      r#"{"counts":{"excellent":"seven"}}"#,
      r#"{"counts":{"excellent":0}}"#,
      r#"{"result":{"grade":"good","issues_found":2},"issues":[]}"#,
      "not json at all",
    ] {
      assert!(parse_round_summary(text).is_none(), "must not read as a round report: {text}");
    }
  }

  #[test]
  fn job_rounds_orders_cycles_and_prefers_the_retried_attempt() {
    let _env = crate::runtime::test_env_lock();
    let dir = std::env::temp_dir().join(format!("scsh-rounds-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, body: &str| {
      let path = dir.join(name);
      std::fs::write(&path, body).unwrap();
      Some(path.to_string_lossy().into_owned())
    };
    let round = |mean: f64, verdict: &str| {
      format!(r#"{{"verdict":"{verdict}","approved":false,"feedback":{{"mean":{mean},"counts":{{"good":1}}}}}}"#)
    };
    let proc = |index: usize, skill: &str, result: Option<String>| ProcRecord {
      index,
      previous_attempt: None,
      label: format!("codex: {skill}"),
      kind: ProcKind::Skill,
      status: ProcStatus::Ok,
      skill_name: Some(skill.into()),
      harness: Some("codex".into()),
      model: None,
      started_at: None,
      note: None,
      detail: None,
      fail_reason: None,
      elapsed: None,
      lines: vec![],
      container_name: None,
      container_runtime: None,
      cast_path: None,
      diff_path: None,
      skill_source: Some("collect".into()),
      route: None,
      result_path: result,
      annotate_target: None,
    };
    let procs = vec![
      // Registered out of order, to prove the trajectory is sorted by cycle.
      proc(0, "collect-while-collect-2", write("c2.json", &round(4.10, "not met"))),
      proc(1, "collect-while-collect-1", write("c1.json", &round(3.60, "not met"))),
      // A cycle that was retried: the later proc is that cycle's authoritative score.
      proc(2, "collect-while-collect-3", write("c3-first.json", &round(1.00, "not met"))),
      proc(3, "collect-while-collect-3", write("c3-retry.json", &round(4.27, "not met"))),
      // Steps that are not loop cycles, and a cycle whose result is not a round report.
      proc(4, "prepare", write("prep.json", r#"{"mean":9.9,"counts":{"good":1}}"#)),
      proc(5, "fix-while-collect-3", write("fix.json", r#"{"message":"applied"}"#)),
      proc(6, "collect-while-collect-4", None),
    ];
    let rounds = job_rounds(&procs);
    assert_eq!(rounds.iter().map(|r| r.iteration).collect::<Vec<_>>(), vec![1, 2, 3], "cycles, oldest first");
    assert_eq!(rounds.iter().map(|r| r.mean.unwrap()).collect::<Vec<_>>(), vec![3.60, 4.10, 4.27]);
    assert_eq!(rounds[2].proc_index, 3, "the retried attempt supersedes the first");
    assert!(rounds.iter().all(|r| r.step == "collect"));
    assert_eq!(rounds[0].verdict.as_deref(), Some("not met"));

    // The API payload carries the same trajectory.
    let payload = fleet_json("rnd001", &procs);
    assert!(payload.contains("\"rounds\": [{ \"iteration\": 1"), "rounds ride the fleet payload: {payload}");
    assert!(payload.contains("\"mean\": 4.27"), "{payload}");
    assert!(crate::json::parse(&payload).is_ok(), "fleet payload stays valid JSON: {payload}");

    // A job with no loop at all reports no rounds.
    assert!(job_rounds(&[proc(0, "prepare", write("p.json", &round(4.0, "met")))]).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn persist_and_rollup_roundtrip() {
    let _env = crate::runtime::test_env_lock();
    let home = std::env::temp_dir().join(format!("scsh-fleet-{}", crate::runtime::random_nonce_6()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let prev = std::env::var_os("SCSH_HOME");
    std::env::set_var("SCSH_HOME", &home);
    let session = "fleet1";
    let result = home.join("tmp-result.json");
    std::fs::write(&result, r#"{"result":"2 + 3 = 5"}"#).unwrap();
    let p1 = persist_skill_result(session, "add-opencode", &result).unwrap();
    let p2_src = home.join("tmp-result2.json");
    std::fs::write(&p2_src, r#"{"result":"2 + 3 = 5"}"#).unwrap();
    let p2 = persist_skill_result(session, "add-claude", &p2_src).unwrap();
    let procs = vec![
      ProcRecord {
        index: 0,
        previous_attempt: None,
        label: "opencode: add-opencode".into(),
        kind: ProcKind::Skill,
        status: ProcStatus::Ok,
        skill_name: Some("add-opencode".into()),
        harness: Some("opencode".into()),
        model: None,
        started_at: None,
        note: None,
        detail: Some("2 + 3 = 5".into()),
        fail_reason: None,
        elapsed: Some(1.0),
        lines: vec![],
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: Some("add".into()),
        route: Some("opencode".into()),
        result_path: Some(p1),
        annotate_target: None,
      },
      ProcRecord {
        index: 1,
        previous_attempt: None,
        label: "claude: add-claude".into(),
        kind: ProcKind::Skill,
        status: ProcStatus::Ok,
        skill_name: Some("add-claude".into()),
        harness: Some("claude".into()),
        model: None,
        started_at: None,
        note: None,
        detail: Some("2 + 3 = 5".into()),
        fail_reason: None,
        elapsed: Some(1.2),
        lines: vec![],
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: Some("add".into()),
        route: Some("claude".into()),
        result_path: Some(p2),
        annotate_target: None,
      },
    ];
    let written = write_rollups(session, &procs);
    assert_eq!(written.len(), 1);
    let body = std::fs::read_to_string(&written[0]).unwrap();
    assert!(body.contains("\"agree\": true"), "{body}");
    let _ = std::fs::remove_dir_all(&home);
    match prev {
      Some(v) => std::env::set_var("SCSH_HOME", v),
      None => std::env::remove_var("SCSH_HOME"),
    }
  }
}
