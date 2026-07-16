//! Durable run statistics — one JSON line per skill invocation (and one per run) in
//! `~/.scsh/stats.jsonl`, browsable with `scsh stats`. Unlike the daemon state and
//! `failures.log` (which live under the volatile `$TMPDIR`), stats survive reboots so
//! questions like "how long do code reviews of N commits / M changed lines take per
//! harness·model route" can be answered across weeks of runs.
//!
//! The schema is deliberately generic: every record carries the repo *workload* at run
//! time (commits and LOC the current branch adds over the repo's main branch), so any
//! future agent — not just code reviewers — gets duration-vs-size data for free.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::json::{parse, quote, Value};

/// Override the stats file location (mainly for tests).
pub const STATS_FILE_ENV: &str = "SCSH_STATS_FILE";

/// The append-only JSONL stats file: `~/.scsh/stats.jsonl` (or `$SCSH_STATS_FILE`).
pub fn stats_path() -> PathBuf {
  if let Some(p) = std::env::var_os(STATS_FILE_ENV).filter(|p| !p.is_empty()) {
    return PathBuf::from(p);
  }
  match std::env::var_os("HOME") {
    Some(h) => PathBuf::from(h).join(".scsh").join("stats.jsonl"),
    None => std::env::temp_dir().join("scsh-stats.jsonl"),
  }
}

/// The size of the work a run processes: what the current branch adds over the repo's
/// main branch (merge-base of HEAD with `main`, falling back to `master`). On main
/// itself — or when no base branch exists — everything is zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Workload {
  pub commits: u64,
  pub loc_added: u64,
  pub loc_deleted: u64,
}

/// Measure the repo's workload (best-effort; zeros when git or a base branch is missing).
pub fn workload_of_repo(root: &Path) -> Workload {
  let Some(base) = merge_base(root) else {
    return Workload::default();
  };
  let commits = git_capture(root, &["rev-list", "--count", &format!("{base}..HEAD")])
    .and_then(|s| s.trim().parse::<u64>().ok())
    .unwrap_or(0);
  let (loc_added, loc_deleted) =
    git_capture(root, &["diff", "--numstat", &format!("{base}..HEAD")]).map(|out| sum_numstat(&out)).unwrap_or((0, 0));
  Workload { commits, loc_added, loc_deleted }
}

fn merge_base(root: &Path) -> Option<String> {
  for base in ["main", "master"] {
    if git_capture(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{base}")]).is_none() {
      continue;
    }
    // On the base branch itself the workload is zero by definition — report no base.
    let head = git_capture(root, &["rev-parse", "HEAD"])?;
    if let Some(mb) = git_capture(root, &["merge-base", "HEAD", base]) {
      let mb = mb.trim().to_string();
      if mb == head.trim() {
        return None;
      }
      return Some(mb);
    }
  }
  None
}

/// Sum a `git diff --numstat` output into `(added, deleted)`; binary rows (`-`) count 0.
pub fn sum_numstat(numstat: &str) -> (u64, u64) {
  let mut added = 0u64;
  let mut deleted = 0u64;
  for line in numstat.lines() {
    let mut cols = line.split_whitespace();
    let (a, d) = (cols.next().unwrap_or("-"), cols.next().unwrap_or("-"));
    added += a.parse::<u64>().unwrap_or(0);
    deleted += d.parse::<u64>().unwrap_or(0);
  }
  (added, deleted)
}

fn git_capture(root: &Path, args: &[&str]) -> Option<String> {
  let out = std::process::Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
  if !out.status.success() {
    return None;
  }
  Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One line of `stats.jsonl`: a finished skill invocation (`kind == "skill"`) or a whole
/// run rollup (`kind == "run"`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatRecord {
  pub ts: u64,
  /// `skill` | `run`.
  pub kind: String,
  pub session: String,
  pub repo: String,
  pub branch: String,
  pub profile: Option<String>,
  /// Resolved invocation name (e.g. `conventions-reviewer-codex-terra`); skill rows only.
  pub skill: Option<String>,
  /// The `.skills/<dir>` the invocation ran (e.g. `conventions-reviewer`); skill rows only.
  pub skill_source: Option<String>,
  pub harness: Option<String>,
  pub model: Option<String>,
  /// Reasoning effort (`.scsh.yml` `effort:`), when the route set one; skill rows only.
  pub effort: Option<String>,
  /// `ok` | `fail` | `cached`; skill rows only.
  pub outcome: Option<String>,
  pub fail_reason: Option<String>,
  /// How many times the route ran: 1, plus the automatic transient-failure retry, plus
  /// one per browser Force restart; skill rows only.
  pub attempts: u64,
  /// Wall-clock seconds of the (final) attempt, or of the whole run for run rows.
  pub duration_secs: f64,
  pub commits: u64,
  pub loc_added: u64,
  pub loc_deleted: u64,
  /// Run rows only.
  pub skills_total: Option<u64>,
  pub skills_failed: Option<u64>,
}

/// Append one record (best-effort: stats never fail a run).
pub fn record(rec: &StatRecord) {
  let path = stats_path();
  if let Some(parent) = path.parent() {
    if std::fs::create_dir_all(parent).is_err() {
      return;
    }
  }
  if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
    let _ = writeln!(f, "{}", record_json(rec));
  }
}

fn record_json(rec: &StatRecord) -> String {
  let mut fields = vec![
    format!("\"ts\": {}", rec.ts),
    format!("\"kind\": {}", quote(&rec.kind)),
    format!("\"session\": {}", quote(&rec.session)),
    format!("\"repo\": {}", quote(&rec.repo)),
    format!("\"branch\": {}", quote(&rec.branch)),
  ];
  let opts: [(&str, &Option<String>); 7] = [
    ("profile", &rec.profile),
    ("skill", &rec.skill),
    ("skill_source", &rec.skill_source),
    ("harness", &rec.harness),
    ("model", &rec.model),
    ("effort", &rec.effort),
    ("outcome", &rec.outcome),
  ];
  for (key, v) in opts {
    if let Some(s) = v {
      fields.push(format!("{}: {}", quote(key), quote(s)));
    }
  }
  if let Some(r) = &rec.fail_reason {
    fields.push(format!("\"fail_reason\": {}", quote(r)));
  }
  fields.push(format!("\"attempts\": {}", rec.attempts));
  fields.push(format!("\"duration_secs\": {:.3}", rec.duration_secs));
  fields.push(format!("\"commits\": {}", rec.commits));
  fields.push(format!("\"loc_added\": {}", rec.loc_added));
  fields.push(format!("\"loc_deleted\": {}", rec.loc_deleted));
  if let Some(n) = rec.skills_total {
    fields.push(format!("\"skills_total\": {n}"));
  }
  if let Some(n) = rec.skills_failed {
    fields.push(format!("\"skills_failed\": {n}"));
  }
  format!("{{ {} }}", fields.join(", "))
}

/// Parse every JSONL line of the stats file, skipping blank or unparseable lines.
pub fn read_records() -> Vec<StatRecord> {
  read_records_from(&stats_path())
}

pub fn read_records_from(path: &Path) -> Vec<StatRecord> {
  let Ok(text) = std::fs::read_to_string(path) else {
    return Vec::new();
  };
  text.lines().filter_map(parse_record).collect()
}

fn parse_record(line: &str) -> Option<StatRecord> {
  let obj = match parse(line.trim()).ok()? {
    Value::Object(o) => o,
    _ => return None,
  };
  let get = |key: &str| {
    obj.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
      Value::String(s) => Some(s.clone()),
      _ => None,
    })
  };
  let num = |key: &str| {
    obj.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
      Value::Number(n) => Some(*n),
      _ => None,
    })
  };
  Some(StatRecord {
    ts: num("ts")? as u64,
    kind: get("kind")?,
    session: get("session").unwrap_or_default(),
    repo: get("repo").unwrap_or_default(),
    branch: get("branch").unwrap_or_default(),
    profile: get("profile"),
    skill: get("skill"),
    skill_source: get("skill_source"),
    harness: get("harness"),
    model: get("model"),
    effort: get("effort"),
    outcome: get("outcome"),
    fail_reason: get("fail_reason"),
    attempts: num("attempts").unwrap_or(1.0) as u64,
    duration_secs: num("duration_secs").unwrap_or(0.0),
    commits: num("commits").unwrap_or(0.0) as u64,
    loc_added: num("loc_added").unwrap_or(0.0) as u64,
    loc_deleted: num("loc_deleted").unwrap_or(0.0) as u64,
    skills_total: num("skills_total").map(|n| n as u64),
    skills_failed: num("skills_failed").map(|n| n as u64),
  })
}

/// Aggregate over one group of skill records: counts and duration/workload averages.
/// Cached hits are counted but excluded from duration and workload averages (they skip
/// the container entirely and would drag every mean toward zero).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkillAggregate {
  pub runs: usize,
  pub ok: usize,
  pub failed: usize,
  pub cached: usize,
  pub retried: usize,
  pub mean_secs: f64,
  pub min_secs: f64,
  pub max_secs: f64,
  pub mean_commits: f64,
  pub mean_loc: f64,
}

pub fn aggregate_skills(records: &[&StatRecord]) -> SkillAggregate {
  let mut agg = SkillAggregate { min_secs: f64::MAX, ..Default::default() };
  let mut timed = 0usize;
  for r in records {
    agg.runs += 1;
    match r.outcome.as_deref() {
      Some("cached") => {
        agg.cached += 1;
        continue;
      }
      Some("ok") => agg.ok += 1,
      _ => agg.failed += 1,
    }
    if r.attempts > 1 {
      agg.retried += 1;
    }
    timed += 1;
    agg.mean_secs += r.duration_secs;
    agg.min_secs = agg.min_secs.min(r.duration_secs);
    agg.max_secs = agg.max_secs.max(r.duration_secs);
    agg.mean_commits += r.commits as f64;
    agg.mean_loc += r.loc_total() as f64;
  }
  if timed > 0 {
    agg.mean_secs /= timed as f64;
    agg.mean_commits /= timed as f64;
    agg.mean_loc /= timed as f64;
  } else {
    agg.min_secs = 0.0;
  }
  agg
}

impl StatRecord {
  pub fn loc_total(&self) -> u64 {
    self.loc_added + self.loc_deleted
  }

  /// Human route label: `harness · model` plus the effort level when one was set.
  pub fn route_label(&self) -> String {
    let mut label =
      format!("{} · {}", self.harness.as_deref().unwrap_or("?"), self.model.as_deref().unwrap_or("(harness default)"));
    if let Some(e) = &self.effort {
      label.push_str(&format!(" ({e})"));
    }
    label
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn sample_skill(outcome: &str, secs: f64, commits: u64, loc: u64, attempts: u64) -> StatRecord {
    StatRecord {
      ts: 1000,
      kind: "skill".into(),
      session: "abc".into(),
      repo: "/r".into(),
      branch: "feature".into(),
      profile: Some("code-review".into()),
      skill: Some("conventions-reviewer-codex-terra".into()),
      skill_source: Some("conventions-reviewer".into()),
      harness: Some("codex".into()),
      model: Some("gpt-5.6-terra".into()),
      effort: Some("high".into()),
      outcome: Some(outcome.into()),
      fail_reason: (outcome == "fail").then(|| "container_timeout".to_string()),
      attempts,
      duration_secs: secs,
      commits,
      loc_added: loc,
      loc_deleted: 0,
      skills_total: None,
      skills_failed: None,
    }
  }

  #[test]
  fn record_json_roundtrips_through_parse_record() {
    let rec = sample_skill("ok", 412.5, 3, 245, 2);
    let back = parse_record(&record_json(&rec)).unwrap();
    assert_eq!(back, rec);
    let run = StatRecord {
      ts: 2000,
      kind: "run".into(),
      session: "abc".into(),
      repo: "/r".into(),
      branch: "feature".into(),
      profile: Some("code-review".into()),
      attempts: 1,
      duration_secs: 900.0,
      commits: 3,
      loc_added: 200,
      loc_deleted: 45,
      skills_total: Some(10),
      skills_failed: Some(1),
      ..Default::default()
    };
    let back = parse_record(&record_json(&run)).unwrap();
    assert_eq!(back, run);
  }

  #[test]
  fn route_label_includes_effort_when_set() {
    let rec = sample_skill("ok", 1.0, 0, 0, 1);
    assert_eq!(rec.route_label(), "codex · gpt-5.6-terra (high)");
    let plain = StatRecord { harness: Some("claude".into()), model: Some("opus".into()), ..Default::default() };
    assert_eq!(plain.route_label(), "claude · opus");
  }

  #[test]
  fn parse_record_skips_garbage_lines() {
    assert!(parse_record("").is_none());
    assert!(parse_record("not json").is_none());
    assert!(parse_record("[1,2,3]").is_none());
  }

  #[test]
  fn sum_numstat_handles_binary_rows() {
    let out = "10\t2\tsrc/a.rs\n-\t-\timg.png\n5\t0\tsrc/b.rs\n";
    assert_eq!(sum_numstat(out), (15, 2));
  }

  #[test]
  fn aggregate_excludes_cached_from_duration_means() {
    let a = sample_skill("ok", 100.0, 2, 50, 1);
    let b = sample_skill("fail", 300.0, 2, 50, 2);
    let c = sample_skill("cached", 0.0, 2, 50, 1);
    let refs: Vec<&StatRecord> = vec![&a, &b, &c];
    let agg = aggregate_skills(&refs);
    assert_eq!((agg.runs, agg.ok, agg.failed, agg.cached, agg.retried), (3, 1, 1, 1, 1));
    assert_eq!(agg.mean_secs, 200.0);
    assert_eq!(agg.min_secs, 100.0);
    assert_eq!(agg.max_secs, 300.0);
    assert_eq!(agg.mean_commits, 2.0);
    assert_eq!(agg.mean_loc, 50.0);
  }

  #[test]
  fn workload_of_repo_zero_outside_git() {
    let dir = std::env::temp_dir().join(format!("scsh-stats-nogit-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    assert_eq!(workload_of_repo(&dir), Workload::default());
    let _ = std::fs::remove_dir_all(&dir);
  }
}
