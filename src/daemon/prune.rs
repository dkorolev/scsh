//! Backup janitor for `/tmp/scsh-*-run-*` dirs — the `scsh run` client deletes first;
//! the daemon retries later only when the dir still exists and the container is gone.

use std::path::Path;

use super::paths::prune_file;
use crate::json::{parse, quote, Value};
use crate::runtime;

/// Seconds after schedule before a successful run dir may be removed (bind-mount teardown).
pub const PRUNE_GRACE_SECS: u64 = 60;

/// Failed run dirs — same retention as the CLI stale sweep in `main.rs`.
pub const PRUNE_FAIL_RETENTION_SECS: u64 = 24 * 60 * 60;

const MAX_JOBS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneJob {
  pub run_dir: String,
  pub container_name: String,
  /// Empty → probe docker, podman, and Apple `container` on tick.
  pub runtime: String,
  pub outcome_ok: bool,
  pub scheduled_at: u64,
  pub eligible_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PruneQueue {
  pub jobs: Vec<PruneJob>,
}

impl PruneQueue {
  pub fn load(port: u16) -> PruneQueue {
    let path = prune_file(port);
    let Ok(text) = std::fs::read_to_string(&path) else {
      return PruneQueue::default();
    };
    parse_queue(&text).unwrap_or_default()
  }

  pub fn save(&self, port: u16) {
    let path = prune_file(port);
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/tmp")));
    let _ = crate::atomic_write(&path, save_queue(self).as_bytes());
  }

  /// Enqueue a backup delete. Idempotent per `run_dir`. Returns false when skipped (`SCSH_KEEP_RUNS`).
  pub fn schedule(&mut self, run_dir: &str, container_name: &str, runtime: &str, outcome_ok: bool, now: u64) -> bool {
    if keep_run_dirs() {
      return false;
    }
    if run_dir.is_empty() || container_name.is_empty() {
      return false;
    }
    if !is_scsh_run_dir_path(run_dir) {
      return false;
    }
    if self.jobs.iter().any(|j| j.run_dir == run_dir) {
      return true;
    }
    let eligible_at =
      if outcome_ok { now.saturating_add(PRUNE_GRACE_SECS) } else { now.saturating_add(PRUNE_FAIL_RETENTION_SECS) };
    self.jobs.push(PruneJob {
      run_dir: run_dir.to_string(),
      container_name: container_name.to_string(),
      runtime: runtime.to_string(),
      outcome_ok,
      scheduled_at: now,
      eligible_at,
    });
    trim_jobs(self);
    true
  }

  /// Advance the queue: delete eligible dirs that still exist. Returns how many were removed.
  pub fn tick(&mut self, now: u64) -> usize {
    if keep_run_dirs() {
      return 0;
    }
    let mut removed = 0;
    let mut remaining = Vec::with_capacity(self.jobs.len());
    for job in self.jobs.drain(..) {
      if now < job.eligible_at {
        remaining.push(job);
        continue;
      }
      if !Path::new(&job.run_dir).is_dir() {
        continue;
      }
      if container_still_present(&job) {
        remaining.push(job);
        continue;
      }
      if std::fs::remove_dir_all(&job.run_dir).is_ok() {
        removed += 1;
      } else {
        remaining.push(job);
      }
    }
    self.jobs = remaining;
    removed
  }
}

fn keep_run_dirs() -> bool {
  matches!(std::env::var("SCSH_KEEP_RUNS").ok().as_deref(), Some("1") | Some("true"))
}

fn is_scsh_run_dir_path(run_dir: &str) -> bool {
  Path::new(run_dir).file_name().and_then(|n| n.to_str()).is_some_and(runtime::is_scsh_run_dir_name)
}

fn trim_jobs(queue: &mut PruneQueue) {
  while queue.jobs.len() > MAX_JOBS {
    queue.jobs.remove(0);
  }
}

fn container_still_present(job: &PruneJob) -> bool {
  if job.runtime.is_empty() {
    runtime::container_named_exists_any(&job.container_name)
  } else {
    runtime::container_named_exists(&job.runtime, &job.container_name)
  }
}

pub fn schedule_from_api(body: &str, queue: &mut PruneQueue, now: u64) -> bool {
  let obj = match parse(body).ok() {
    Some(Value::Object(o)) => o,
    _ => return false,
  };
  let run_dir = field_str(&obj, "run_dir").unwrap_or_default();
  let container_name = field_str(&obj, "container_name").unwrap_or_default();
  let runtime = field_str(&obj, "runtime").unwrap_or_default();
  let outcome = field_str(&obj, "outcome").unwrap_or_default();
  let outcome_ok = outcome == "ok";
  if run_dir.is_empty() || container_name.is_empty() {
    return false;
  }
  queue.schedule(&run_dir, &container_name, &runtime, outcome_ok, now)
}

pub fn schedule_orphans_from_session(queue: &mut PruneQueue, container_names: &[(String, String)], now: u64) {
  for (name, runtime) in container_names {
    let run_dir = format!("/tmp/{name}");
    queue.schedule(&run_dir, name, runtime, false, now);
  }
}

fn field_str(obj: &[(String, Value)], key: &str) -> Option<String> {
  obj.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
    Value::String(s) => Some(s.clone()),
    _ => None,
  })
}

fn parse_queue(text: &str) -> Result<PruneQueue, String> {
  let root = parse(text)?;
  let obj = match root {
    Value::Object(o) => o,
    _ => return Err("expected object".into()),
  };
  let jobs = match obj.iter().find(|(k, _)| k == "jobs").map(|(_, v)| v) {
    Some(Value::Array(arr)) => arr.iter().filter_map(parse_job).collect(),
    _ => Vec::new(),
  };
  Ok(PruneQueue { jobs })
}

fn parse_job(v: &Value) -> Option<PruneJob> {
  let obj = match v {
    Value::Object(o) => o,
    _ => return None,
  };
  Some(PruneJob {
    run_dir: field_str(obj, "run_dir")?,
    container_name: field_str(obj, "container_name")?,
    runtime: field_str(obj, "runtime").unwrap_or_default(),
    outcome_ok: field_str(obj, "outcome").as_deref() == Some("ok"),
    scheduled_at: field_num(obj, "scheduled_at")? as u64,
    eligible_at: field_num(obj, "eligible_at")? as u64,
  })
}

fn field_num(obj: &[(String, Value)], key: &str) -> Option<f64> {
  obj.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
    Value::Number(n) => Some(*n),
    _ => None,
  })
}

pub fn save_queue(queue: &PruneQueue) -> String {
  let parts: Vec<String> = queue.jobs.iter().map(job_json).collect();
  format!("{{\n  \"jobs\": [\n    {}\n  ]\n}}", parts.join(",\n    "))
}

fn job_json(j: &PruneJob) -> String {
  let outcome = if j.outcome_ok { "ok" } else { "fail" };
  format!(
    "{{ \"run_dir\": {}, \"container_name\": {}, \"runtime\": {}, \"outcome\": {}, \
\"scheduled_at\": {}, \"eligible_at\": {} }}",
    quote(&j.run_dir),
    quote(&j.container_name),
    quote(&j.runtime),
    quote(outcome),
    j.scheduled_at,
    j.eligible_at,
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn schedule_dedupes_by_run_dir() {
    let mut q = PruneQueue::default();
    let now = 1_000;
    assert!(q.schedule("/tmp/scsh-abcdef-run-add", "scsh-abcdef-run-add", "docker", true, now));
    assert!(q.schedule("/tmp/scsh-abcdef-run-add", "scsh-abcdef-run-add", "docker", true, now));
    assert_eq!(q.jobs.len(), 1);
  }

  #[test]
  fn ok_jobs_eligible_after_grace() {
    let name = "scsh-abcdef-run-add";
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let run_dir = dir.to_string_lossy().into_owned();
    let now = 5_000;
    let mut q = PruneQueue::default();
    q.schedule(&run_dir, name, "docker", true, now);
    assert_eq!(q.tick(now + PRUNE_GRACE_SECS - 1), 0);
    assert_eq!(q.tick(now + PRUNE_GRACE_SECS), 1);
    assert!(!dir.exists());
  }

  #[test]
  fn missing_dir_drops_job_without_error() {
    let mut q = PruneQueue::default();
    q.schedule("/tmp/scsh-no-such-run-add", "scsh-no-such-run-add", "docker", true, 0);
    assert_eq!(q.tick(PRUNE_GRACE_SECS + 1), 0);
    assert!(q.jobs.is_empty());
  }

  #[test]
  fn roundtrip_json() {
    let mut q = PruneQueue::default();
    q.schedule("/tmp/scsh-abcdef-run-add", "scsh-abcdef-run-add", "docker", false, 100);
    let text = save_queue(&q);
    let back = parse_queue(&text).unwrap();
    assert_eq!(back, q);
  }
}
