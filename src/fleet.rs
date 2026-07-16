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

/// Copy a skill's result JSON into `$SCSH_HOME/sessions/<id>/results/<invocation>.json`.
pub fn persist_skill_result(session_id: &str, skill_name: &str, host_result: &Path) -> Option<String> {
  if !host_result.is_file() {
    return None;
  }
  let dir = runtime::session_results_dir(session_id);
  std::fs::create_dir_all(&dir).ok()?;
  let safe = skill_name.replace('/', "_");
  let dest = dir.join(format!("{safe}.json"));
  std::fs::copy(host_result, &dest).ok()?;
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

/// Write deterministic rollup JSON for every multi-route skill_source.
pub fn write_rollups(session_id: &str, procs: &[ProcRecord]) -> Vec<PathBuf> {
  let groups = fleet_groups(procs);
  let mut written = Vec::new();
  let dir = runtime::session_results_dir(session_id);
  let _ = std::fs::create_dir_all(&dir);
  for g in groups {
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
    let body = format!(
      "{{\n  \"skill_source\": {},\n  \"ok\": {ok},\n  \"fail\": {fail},\n  \"agree\": {},\n  \"issues_total\": {issues_total},\n  \"grades\": [{}],\n  \"routes\": [\n    {}\n  ]\n}}\n",
      json::quote(&g.skill_source),
      if agree { "true" } else { "false" },
      grades.iter().map(|gr| json::quote(gr)).collect::<Vec<_>>().join(", "),
      routes_json.join(",\n    "),
    );
    let path = dir.join(format!("{}-rollup.json", g.skill_source));
    if std::fs::write(&path, body).is_ok() {
      written.push(path);
    }
  }
  written
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
  Some(ResultSummary { message, grade, comments_count, issues_found })
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
