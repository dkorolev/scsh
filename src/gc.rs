//! `scsh gc` — reclaim disk under `$SCSH_HOME/sessions/` (and optional legacy top-level
//! `casts/` / `recordings/`). Dry-run by default; `--apply` required to delete. Never
//! touches `projects/`, `stats.jsonl`, redb files, or other non-session paths.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default: keep the 50 newest session dirs (by directory mtime).
pub const DEFAULT_KEEP: usize = 50;
/// Default: only consider dirs older than 30 days.
pub const DEFAULT_DAYS: u64 = 30;

#[derive(Debug, Clone)]
pub struct GcOpts {
  pub apply: bool,
  pub days: u64,
  pub keep: usize,
  pub legacy: bool,
}

impl Default for GcOpts {
  fn default() -> Self {
    Self { apply: false, days: DEFAULT_DAYS, keep: DEFAULT_KEEP, legacy: false }
  }
}

/// One path that would be (or was) removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
  pub path: PathBuf,
  pub bytes: u64,
  pub mtime_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GcPlan {
  pub candidates: Vec<Candidate>,
  pub total_bytes: u64,
}

/// Scan `home` and build the deletion plan. `now_secs` is injected so tests can pin time.
pub fn plan(home: &Path, opts: &GcOpts, now_secs: u64) -> GcPlan {
  let mut entries = list_session_dirs(&home.join("sessions"));
  entries.sort_by(|a, b| b.mtime_secs.cmp(&a.mtime_secs).then_with(|| a.path.cmp(&b.path)));
  let cutoff = now_secs.saturating_sub(opts.days.saturating_mul(24 * 60 * 60));
  let mut candidates: Vec<Candidate> = entries
    .into_iter()
    .enumerate()
    .filter(|(i, e)| *i >= opts.keep && e.mtime_secs <= cutoff)
    .map(|(_, e)| e)
    .collect();
  if opts.legacy {
    for name in ["casts", "recordings"] {
      let path = home.join(name);
      if path.is_dir() {
        let bytes = dir_size(&path);
        let mtime_secs = mtime_secs(&path).unwrap_or(0);
        candidates.push(Candidate { path, bytes, mtime_secs });
      }
    }
  }
  let total_bytes = candidates.iter().map(|c| c.bytes).sum();
  GcPlan { candidates, total_bytes }
}

/// Delete every candidate. Returns bytes successfully freed (skips paths that fail).
pub fn apply_plan(plan: &GcPlan) -> u64 {
  let mut freed = 0u64;
  for c in &plan.candidates {
    if std::fs::remove_dir_all(&c.path).is_ok() {
      freed = freed.saturating_add(c.bytes);
    }
  }
  freed
}

/// Human size: `12B`, `1.5kB`, `2.0MB`, …
pub fn human_bytes(bytes: u64) -> String {
  const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
  let mut value = bytes as f64;
  let mut unit = 0;
  while value >= 1000.0 && unit + 1 < UNITS.len() {
    value /= 1000.0;
    unit += 1;
  }
  if unit == 0 {
    format!("{bytes}B")
  } else {
    format!("{value:.1}{}", UNITS[unit])
  }
}

/// Recursive byte size of a directory tree (files only; best-effort).
pub fn dir_size(path: &Path) -> u64 {
  let mut total = 0u64;
  let Ok(entries) = std::fs::read_dir(path) else {
    return 0;
  };
  for entry in entries.flatten() {
    let p = entry.path();
    let Ok(meta) = entry.metadata() else {
      continue;
    };
    if meta.is_dir() {
      total = total.saturating_add(dir_size(&p));
    } else if meta.is_file() {
      total = total.saturating_add(meta.len());
    }
  }
  total
}

fn list_session_dirs(sessions: &Path) -> Vec<Candidate> {
  let Ok(entries) = std::fs::read_dir(sessions) else {
    return Vec::new();
  };
  let mut out = Vec::new();
  for entry in entries.flatten() {
    let path = entry.path();
    if !path.is_dir() {
      continue;
    }
    let Some(mtime_secs) = mtime_secs(&path) else {
      continue;
    };
    out.push(Candidate { bytes: dir_size(&path), mtime_secs, path });
  }
  out
}

fn mtime_secs(path: &Path) -> Option<u64> {
  std::fs::metadata(path)
    .and_then(|m| m.modified())
    .ok()
    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
    .map(|d| d.as_secs())
}

/// Unix epoch seconds for "now" (tests inject their own).
pub fn now_unix_secs() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use std::time::Duration;

  fn tmp_home(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("scsh-gc-{tag}-{}-{}", std::process::id(), now_unix_secs()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(p.join("sessions")).unwrap();
    p
  }

  fn write_session(home: &Path, id: &str, payload: &[u8]) -> PathBuf {
    let dir = home.join("sessions").join(id);
    fs::create_dir_all(dir.join("casts")).unwrap();
    fs::write(dir.join("casts").join("x.cast"), payload).unwrap();
    dir
  }

  fn set_mtime(path: &Path, unix_secs: u64) {
    let t = UNIX_EPOCH + Duration::from_secs(unix_secs);
    let f = fs::File::open(path).unwrap();
    f.set_modified(t).unwrap();
  }

  #[test]
  fn dry_run_lists_old_dirs_beyond_keep() {
    let home = tmp_home("dry");
    let now = 1_700_000_000u64;
    // 3 sessions: two old (beyond keep=1), one newest kept.
    let a = write_session(&home, "aaaaaa", b"aaa");
    let b = write_session(&home, "bbbbbb", b"bbbb");
    let c = write_session(&home, "cccccc", b"c");
    set_mtime(&a, now - 40 * 86400);
    set_mtime(&b, now - 40 * 86400);
    set_mtime(&c, now - 86400);
    // projects/ must never appear as a candidate.
    fs::create_dir_all(home.join("projects").join("demo")).unwrap();
    fs::write(home.join("projects").join("demo").join("x"), b"keep").unwrap();
    fs::write(home.join("stats.jsonl"), b"{}\n").unwrap();

    let opts = GcOpts { apply: false, days: 30, keep: 1, legacy: false };
    let plan = plan(&home, &opts, now);
    assert_eq!(plan.candidates.len(), 2);
    let paths: Vec<_> = plan.candidates.iter().map(|c| c.path.clone()).collect();
    assert!(paths.contains(&a));
    assert!(paths.contains(&b));
    assert!(!paths.iter().any(|p| p.ends_with("cccccc")));
    assert!(!paths.iter().any(|p| p.to_string_lossy().contains("projects")));
    assert!(plan.total_bytes >= 7);
    // Dry-run leaves everything in place.
    assert!(a.is_dir());
    assert!(home.join("projects").join("demo").is_dir());
    assert!(home.join("stats.jsonl").is_file());
    let _ = fs::remove_dir_all(&home);
  }

  #[test]
  fn apply_deletes_candidates() {
    let home = tmp_home("apply");
    let now = 1_700_000_000u64;
    let old = write_session(&home, "oldold", b"deadbeef");
    let keep = write_session(&home, "newnew", b"live");
    set_mtime(&old, now - 60 * 86400);
    set_mtime(&keep, now - 86400);
    let opts = GcOpts { apply: true, days: 30, keep: 1, legacy: false };
    let p = plan(&home, &opts, now);
    assert_eq!(p.candidates.len(), 1);
    assert_eq!(p.candidates[0].path, old);
    let freed = apply_plan(&p);
    assert_eq!(freed, p.total_bytes);
    assert!(!old.exists());
    assert!(keep.is_dir());
    let _ = fs::remove_dir_all(&home);
  }

  #[test]
  fn keep_protects_newest() {
    let home = tmp_home("keep");
    let now = 1_700_000_000u64;
    // All old enough for --days, but keep=2 retains the two newest.
    let mut dirs = Vec::new();
    for (i, id) in ["s00001", "s00002", "s00003"].iter().enumerate() {
      let payload = [b'x', i as u8 + 1];
      let d = write_session(&home, id, &payload);
      set_mtime(&d, now - (50 + i as u64) * 86400);
      dirs.push(d);
    }
    let opts = GcOpts { apply: false, days: 30, keep: 2, legacy: false };
    let p = plan(&home, &opts, now);
    assert_eq!(p.candidates.len(), 1);
    // Oldest mtime is s00003 (50+2 days ago).
    assert_eq!(p.candidates[0].path, dirs[2]);
    let _ = fs::remove_dir_all(&home);
  }

  #[test]
  fn legacy_removes_casts_and_recordings() {
    let home = tmp_home("legacy");
    let now = 1_700_000_000u64;
    fs::create_dir_all(home.join("casts")).unwrap();
    fs::write(home.join("casts").join("old.cast"), b"legacy-cast").unwrap();
    fs::create_dir_all(home.join("recordings")).unwrap();
    fs::write(home.join("recordings").join("r.cast"), b"rec").unwrap();
    fs::create_dir_all(home.join("projects").join("safe")).unwrap();
    fs::write(home.join("projects").join("safe").join("f"), b"nope").unwrap();

    let opts = GcOpts { apply: true, days: 30, keep: 50, legacy: true };
    let p = plan(&home, &opts, now);
    assert_eq!(p.candidates.len(), 2);
    assert!(p.candidates.iter().any(|c| c.path.ends_with("casts")));
    assert!(p.candidates.iter().any(|c| c.path.ends_with("recordings")));
    let freed = apply_plan(&p);
    assert_eq!(freed, p.total_bytes);
    assert!(!home.join("casts").exists());
    assert!(!home.join("recordings").exists());
    assert!(home.join("projects").join("safe").join("f").is_file());
    let _ = fs::remove_dir_all(&home);
  }

  #[test]
  fn never_deletes_projects() {
    let home = tmp_home("projects");
    let now = 1_700_000_000u64;
    fs::create_dir_all(home.join("projects").join("demo")).unwrap();
    fs::write(home.join("projects").join("demo").join("x"), b"keep-me").unwrap();
    // Even with legacy + apply and an empty sessions tree, projects stay.
    let opts = GcOpts { apply: true, days: 0, keep: 0, legacy: true };
    let p = plan(&home, &opts, now);
    assert!(p.candidates.iter().all(|c| !c.path.to_string_lossy().contains("projects")));
    let _ = apply_plan(&p);
    assert!(home.join("projects").join("demo").join("x").is_file());
    let _ = fs::remove_dir_all(&home);
  }

  #[test]
  fn human_bytes_formats() {
    assert_eq!(human_bytes(0), "0B");
    assert_eq!(human_bytes(12), "12B");
    assert_eq!(human_bytes(1_500), "1.5kB");
    assert_eq!(human_bytes(1_500_000), "1.5MB");
  }
}
