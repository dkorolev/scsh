//! Per-attempt cleanup journals. The runner owns finalization while alive; the daemon
//! resumes it after a crash. Journals live outside the disposable, agent-writable clone.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use crate::json::{self, Value};
use crate::runtime;

pub const BUDGET: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
  pub run_dir: PathBuf,
  pub runtime: String,
  pub session: String,
  pub skill: String,
  pub owner_pid: u32,
  pub outputs: Vec<String>,
  pub commit_base: Option<String>,
  pub key_dir: Option<PathBuf>,
  pub container_started: bool,
  pub last_error: Option<String>,
}

fn journal_dir() -> PathBuf {
  runtime::scsh_home().join("cleanup")
}

pub fn journal_path(run_dir: &Path) -> PathBuf {
  journal_dir().join(format!("{}.json", run_dir.file_name().unwrap_or_default().to_string_lossy()))
}

pub fn artifact_stem(run_dir: &Path, skill: &str) -> String {
  format!("{}-{}", runtime::sanitize_component(skill), run_dir.file_name().unwrap_or_default().to_string_lossy())
}

/// The complete diagnostic allowlist. No copied auth directories or dependency caches.
pub fn log_sources() -> Vec<(String, &'static str)> {
  [
    ("", "log"),
    (".debug", "debug.log"),
    (".last", "last.log"),
    (".exit", "exit"),
    (".tuidebug", "tuidebug"),
    (".cursor-hooks.jsonl", "cursor-hooks.jsonl"),
    (".opencode-events.jsonl", "opencode-events.jsonl"),
    (".usage-error", "usage-error"),
    (".usage-final", "usage-final"),
    (".invocation.json", "invocation.json"),
    (".cli-version", "cli-version"),
    (".events.jsonl", "events.jsonl"),
    (".cline-quota-before.limits.json", "cline-quota-before.limits.json"),
    (".cline-quota-before.plan.json", "cline-quota-before.plan.json"),
    (".cline-quota-after.limits.json", "cline-quota-after.limits.json"),
    (".cline-quota-after.plan.json", "cline-quota-after.plan.json"),
  ]
  .into_iter()
  .map(|(suffix, ext)| (format!("{}{suffix}", runtime::RUN_LOG_REL), ext))
  .collect()
}

impl Attempt {
  pub fn save(&self) -> Result<(), String> {
    self.validate()?;
    std::fs::create_dir_all(journal_dir()).map_err(|e| e.to_string())?;
    crate::atomic_write(&journal_path(&self.run_dir), self.json().as_bytes()).map_err(|e| e.to_string())
  }

  fn validate(&self) -> Result<(), String> {
    let name = self.run_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let parent = self.run_dir.parent().and_then(|p| p.canonicalize().ok());
    let system_tmp = Path::new("/tmp").canonicalize().ok();
    if !runtime::is_scsh_run_dir_name(name)
      || parent.is_none()
      || parent != system_tmp
      || self.session.is_empty()
      || !relative_path(&self.session)
      || Path::new(&self.session).components().count() != 1
      || !matches!(self.runtime.as_str(), "docker" | "podman" | "container")
      || self.outputs.iter().any(|p| !relative_path(p))
    {
      return Err("invalid cleanup ownership record".into());
    }
    match std::fs::symlink_metadata(&self.run_dir) {
      Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
        return Err("cleanup directory was replaced".into())
      }
      Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
      _ => {}
    }
    if let Some(key) = &self.key_dir {
      if key.parent() != self.run_dir.parent()
        || !key.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&format!(".{name}.keys-")))
      {
        return Err("invalid cleanup key-channel path".into());
      }
    }
    Ok(())
  }

  /// Call only after host consumers finish, or once the recorded owner has exited.
  pub fn finish(&self, deadline: Instant) -> Result<(), String> {
    let result = self.finish_inner(deadline);
    if let Err(error) = &result {
      let mut pending = self.clone();
      pending.last_error = Some(error.clone());
      if let Err(save_error) = pending.save() {
        return Err(format!("{error}; could not persist cleanup error: {save_error}"));
      }
    }
    result
  }

  fn finish_inner(&self, deadline: Instant) -> Result<(), String> {
    self.validate()?;
    if Instant::now() >= deadline {
      return Err("cleanup deadline exceeded".into());
    }
    let name = self.run_dir.file_name().unwrap().to_string_lossy();
    if self.container_started {
      crate::ui::signals::stop_container_until(&self.runtime, &name, deadline)?;
    }
    self.finish_host(deadline)?;
    remove_file_if_present(&journal_path(&self.run_dir))
  }

  fn finish_host(&self, deadline: Instant) -> Result<(), String> {
    // Clear credentials even if a later diagnostic copy is blocked. No guest is using
    // them now, and these files must never enter a recovery archive.
    if let Some(key) = &self.key_dir {
      remove_dir_if_present(key)?;
    }
    for rel in [
      runtime::CLAUDE_AUTH_REL,
      runtime::OPENCODE_DATA_REL,
      runtime::CODEX_FORWARD_REL,
      runtime::GROK_FORWARD_REL,
      runtime::CURSOR_FORWARD_REL,
      runtime::CURSOR_AUTH_FORWARD_REL,
      runtime::GH_CONFIG_REL,
    ] {
      let path = self.run_dir.join(rel);
      if path.symlink_metadata().is_ok() {
        let parent = path.parent().unwrap().canonicalize().map_err(|e| e.to_string())?;
        if !parent.starts_with(self.run_dir.canonicalize().map_err(|e| e.to_string())?) {
          return Err("credential path escaped its run directory".into());
        }
      }
      remove_dir_if_present(&path)?;
    }
    let stem = artifact_stem(&self.run_dir, &self.skill);
    for (rel, ext) in log_sources() {
      copy_if_present(&self.run_dir, &rel, &runtime::session_logs_dir(&self.session).join(format!("{stem}.{ext}")))?;
    }
    copy_if_present(
      &self.run_dir,
      runtime::RUN_CAST_REL,
      &runtime::session_casts_dir(&self.session).join(format!("{stem}.cast")),
    )?;
    // Keep declared outputs even when collection failed or a parallel workflow step
    // aborted the wave before this attempt's result was consumed.
    for rel in &self.outputs {
      let dest = runtime::session_results_dir(&self.session).join(&stem).join(rel);
      copy_if_present(&self.run_dir, rel, &dest)?;
    }
    self.preserve_commits(deadline)?;
    if Instant::now() >= deadline {
      return Err("cleanup deadline exceeded".into());
    }
    remove_dir_if_present(&self.run_dir)
  }

  fn preserve_commits(&self, deadline: Instant) -> Result<(), String> {
    let Some(base) = &self.commit_base else { return Ok(()) };
    if !self.run_dir.exists() {
      return Ok(());
    }
    let repo = runtime::commits_fetch_path(&self.run_dir);
    if !repo.exists() {
      return Ok(());
    }
    if !repo
      .canonicalize()
      .map_err(|e| e.to_string())?
      .starts_with(self.run_dir.canonicalize().map_err(|e| e.to_string())?)
    {
      return Err("commit repository escaped its run directory".into());
    }
    if !repo.join("HEAD").is_file() && !repo.join(".git").exists() {
      return Ok(());
    }
    let git = |args: &[&str]| {
      let output = runtime::command_output(
        crate::git_command().arg("-C").arg(&repo).args(args),
        deadline.saturating_duration_since(Instant::now()),
      )?;
      if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
      } else {
        Err(format!("commit recovery: {}", String::from_utf8_lossy(&output.stderr).trim()))
      }
    };
    // An empty pull.git is normal when setup or the guest failed before pushing.
    if git(&["rev-parse", "--verify", "HEAD"]).is_err() {
      let refs = git(&["for-each-ref", "--format=%(refname)"])?;
      if refs.is_empty() {
        return Ok(());
      }
      return Err("commit recovery: repository HEAD is unreadable".into());
    }
    let range = format!("{base}..HEAD");
    if git(&["rev-list", "--count", &range])? == "0" {
      return Ok(());
    }
    let dir = runtime::session_results_dir(&self.session);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(format!("{}.bundle", artifact_stem(&self.run_dir, &self.skill)));
    let temporary = dest.with_extension("bundle.partial");
    let result = git(&["bundle", "create", &temporary.to_string_lossy(), &range])
      .and_then(|_| git(&["bundle", "verify", &temporary.to_string_lossy()]))
      .and_then(|_| std::fs::rename(&temporary, &dest).map_err(|e| e.to_string()));
    let _ = std::fs::remove_file(temporary);
    result
  }

  fn json(&self) -> String {
    let outputs = self.outputs.iter().map(|p| json::quote(p)).collect::<Vec<_>>().join(",");
    format!(
      r#"{{"run_dir":{},"runtime":{},"session":{},"skill":{},"owner_pid":{},"outputs":[{}],"commit_base":{},"key_dir":{},"container_started":{},"last_error":{}}}"#,
      json::quote(&self.run_dir.to_string_lossy()),
      json::quote(&self.runtime),
      json::quote(&self.session),
      json::quote(&self.skill),
      self.owner_pid,
      outputs,
      json::quote(self.commit_base.as_deref().unwrap_or("")),
      json::quote(&self.key_dir.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()),
      self.container_started,
      json::quote(self.last_error.as_deref().unwrap_or(""))
    )
  }
}

pub fn load(run_dir: &Path) -> Result<Option<Attempt>, String> {
  match std::fs::read_to_string(journal_path(run_dir)) {
    Ok(text) => parse(&text).map(Some),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
    Err(e) => Err(e.to_string()),
  }
}

fn parse(text: &str) -> Result<Attempt, String> {
  let Value::Object(fields) = json::parse(text)? else { return Err("invalid cleanup record".into()) };
  let string = |key: &str| -> Result<String, String> {
    fields
      .iter()
      .find_map(|(k, v)| match v {
        Value::String(s) if k == key => Some(s.clone()),
        _ => None,
      })
      .ok_or_else(|| format!("missing cleanup field {key}"))
  };
  let owner_pid = fields
    .iter()
    .find_map(|(k, v)| match v {
      Value::Number(n) if k == "owner_pid" && *n > 0.0 && *n <= u32::MAX as f64 && n.fract() == 0.0 => Some(*n as u32),
      _ => None,
    })
    .ok_or("invalid cleanup owner")?;
  let outputs = fields
    .iter()
    .find_map(|(k, v)| match v {
      Value::Array(a) if k == "outputs" => Some(a),
      _ => None,
    })
    .ok_or("missing cleanup outputs")?
    .iter()
    .map(|v| match v {
      Value::String(s) => Ok(s.clone()),
      _ => Err("invalid cleanup output"),
    })
    .collect::<Result<Vec<_>, _>>()?;
  let record = Attempt {
    run_dir: PathBuf::from(string("run_dir")?),
    runtime: string("runtime")?,
    session: string("session")?,
    skill: string("skill")?,
    owner_pid,
    outputs,
    commit_base: Some(string("commit_base")?).filter(|s| !s.is_empty()),
    key_dir: Some(string("key_dir")?).filter(|s| !s.is_empty()).map(PathBuf::from),
    container_started: fields
      .iter()
      .find_map(|(k, v)| match v {
        Value::Bool(b) if k == "container_started" => Some(*b),
        _ => None,
      })
      .ok_or("missing container state")?,
    last_error: Some(string("last_error")?).filter(|s| !s.is_empty()),
  };
  record.validate()?;
  Ok(record)
}

/// Daemon recovery never races a live runner, including its post-container git work.
pub fn recover_pending(deadline: Instant) {
  // OS file locks are released on process death. They also serialize recovery when
  // more than one daemon port, or `scsh prune --now`, shares the same SCSH_HOME.
  let Ok(lock) =
    std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(journal_dir().join("worker.lock"))
  else {
    return;
  };
  if lock.try_lock().is_err() {
    return;
  }
  let Ok(entries) = std::fs::read_dir(journal_dir()) else { return };
  for entry in entries.flatten() {
    if Instant::now() >= deadline {
      break;
    }
    if entry.path().extension().is_none_or(|e| e != "json") {
      continue;
    }
    let result = std::fs::read_to_string(entry.path()).map_err(|e| e.to_string()).and_then(|s| parse(&s));
    match result {
      Ok(attempt) if !crate::daemon::pid_alive(attempt.owner_pid) => {
        if let Err(error) = attempt.finish(deadline) {
          eprintln!("scsh: cleanup pending for {}: {error}", attempt.run_dir.display());
        }
      }
      Ok(_) => {}
      Err(error) => eprintln!("scsh: cannot read cleanup record {}: {error}", entry.path().display()),
    }
  }
}

pub fn pending_descriptions() -> Vec<String> {
  let Ok(entries) = std::fs::read_dir(journal_dir()) else { return Vec::new() };
  entries
    .flatten()
    .filter(|e| e.path().extension().is_some_and(|s| s == "json"))
    .map(|entry| match std::fs::read_to_string(entry.path()).map_err(|e| e.to_string()).and_then(|s| parse(&s)) {
      Ok(attempt) => format!(
        "{}: {}",
        attempt.run_dir.display(),
        attempt.last_error.as_deref().unwrap_or("owned by a running or interrupted attempt")
      ),
      Err(error) => format!("{}: {error}", entry.path().display()),
    })
    .collect()
}

fn relative_path(path: &str) -> bool {
  !path.is_empty() && Path::new(path).components().all(|c| matches!(c, Component::Normal(_)))
}

fn copy_if_present(root: &Path, rel: &str, dest: &Path) -> Result<(), String> {
  let src = root.join(rel);
  match std::fs::symlink_metadata(&src) {
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(e) => return Err(format!("{}: {e}", src.display())),
    Ok(meta) if !meta.is_file() => return Err(format!("{} is not a regular artifact", src.display())),
    Ok(_) => {}
  }
  let actual = src.canonicalize().map_err(|e| e.to_string())?;
  if !actual.starts_with(root.canonicalize().map_err(|e| e.to_string())?) {
    return Err("artifact escaped its run directory".into());
  }
  std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| format!("{}: {e}", dest.display()))?;
  let temporary = dest.with_extension(format!("{}.partial", dest.extension().unwrap_or_default().to_string_lossy()));
  let result = std::fs::copy(&src, &temporary).and_then(|_| std::fs::rename(&temporary, dest));
  let _ = std::fs::remove_file(temporary);
  result.map_err(|e| format!("could not preserve {}: {e}", src.display()))
}

fn remove_file_if_present(path: &Path) -> Result<(), String> {
  match std::fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(e) => Err(format!("could not remove {}: {e}", path.display())),
  }
}

fn remove_dir_if_present(path: &Path) -> Result<(), String> {
  match std::fs::symlink_metadata(path) {
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Ok(meta) if meta.file_type().is_symlink() => remove_file_if_present(path),
    Ok(_) => std::fs::remove_dir_all(path).map_err(|e| format!("could not remove {}: {e}", path.display())),
    Err(e) => Err(format!("could not inspect {}: {e}", path.display())),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct Fixture {
    attempt: Attempt,
    home: PathBuf,
    previous_home: Option<std::ffi::OsString>,
    previous_path: Option<std::ffi::OsString>,
    _env: std::sync::MutexGuard<'static, ()>,
  }

  impl Fixture {
    fn new() -> Self {
      let env = runtime::test_env_lock();
      let run_dir = PathBuf::from("/tmp").join(format!("scsh-{}-run-cleanup-test", runtime::random_nonce_6()));
      std::fs::create_dir(&run_dir).unwrap();
      let home = run_dir.with_extension("home");
      let previous_home = std::env::var_os("SCSH_HOME");
      std::env::set_var("SCSH_HOME", &home);
      let attempt = Attempt {
        run_dir,
        runtime: "docker".into(),
        session: "cleanup-test".into(),
        skill: "test".into(),
        owner_pid: std::process::id(),
        outputs: vec!["tmp/result.json".into()],
        commit_base: None,
        key_dir: None,
        container_started: false,
        last_error: None,
      };
      Self { attempt, home, previous_home, previous_path: std::env::var_os("PATH"), _env: env }
    }

    fn write(&self, rel: &str, data: &str) {
      let path = self.attempt.run_dir.join(rel);
      std::fs::create_dir_all(path.parent().unwrap()).unwrap();
      std::fs::write(path, data).unwrap();
    }

    #[cfg(unix)]
    fn runtime_script(&self, script: &str) {
      use std::os::unix::fs::PermissionsExt;
      let bin = self.home.join("bin");
      std::fs::create_dir_all(&bin).unwrap();
      let path = bin.join("docker");
      std::fs::write(&path, script).unwrap();
      std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
      let mut paths = vec![bin];
      if let Some(previous) = &self.previous_path {
        paths.extend(std::env::split_paths(previous));
      }
      std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      if let Some(key) = &self.attempt.key_dir {
        let _ = std::fs::remove_dir_all(key);
      }
      let _ = std::fs::remove_dir_all(&self.attempt.run_dir);
      let _ = std::fs::remove_dir_all(&self.home);
      match &self.previous_home {
        Some(home) => std::env::set_var("SCSH_HOME", home),
        None => std::env::remove_var("SCSH_HOME"),
      }
      match &self.previous_path {
        Some(path) => std::env::set_var("PATH", path),
        None => std::env::remove_var("PATH"),
      }
    }
  }

  #[test]
  fn setup_failure_is_cleaned_without_a_container_or_clone() {
    let mut f = Fixture::new();
    f.attempt.commit_base = Some("0".repeat(40));
    f.write("partial-clone", "unfinished");
    f.attempt.save().unwrap();
    f.attempt.finish(Instant::now() + BUDGET).unwrap();
    assert!(!f.attempt.run_dir.exists());
    assert!(!journal_path(&f.attempt.run_dir).exists());
    f.attempt.finish(Instant::now() + BUDGET).unwrap();
  }

  #[test]
  fn failed_copy_keeps_source_then_retry_preserves_and_removes_scratch() {
    let mut f = Fixture::new();
    f.write(runtime::RUN_LOG_REL, "failure details");
    f.write(runtime::RUN_CAST_REL, "recording");
    f.write("tmp/result.json", "result");
    f.write("tmp/.claude-auth/token", "credential");
    let key = f
      .attempt
      .run_dir
      .with_file_name(format!(".{}.keys-test", f.attempt.run_dir.file_name().unwrap().to_string_lossy()));
    std::fs::create_dir(&key).unwrap();
    f.attempt.key_dir = Some(key.clone());
    f.attempt.save().unwrap();
    assert_eq!(load(&f.attempt.run_dir).unwrap(), Some(f.attempt.clone()));
    let session = runtime::host_sessions_dir().join(&f.attempt.session);
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(session.join("logs"), "blocks directory creation").unwrap();
    assert!(f.attempt.finish(Instant::now() + BUDGET).is_err());
    assert!(journal_path(&f.attempt.run_dir).is_file());
    assert!(f.attempt.run_dir.join(runtime::RUN_LOG_REL).is_file());
    assert!(!key.exists());
    assert!(!f.attempt.run_dir.join("tmp/.claude-auth").exists());
    std::fs::remove_file(session.join("logs")).unwrap();
    f.attempt.finish(Instant::now() + BUDGET).unwrap();
    assert!(!f.attempt.run_dir.exists());
    let stem = artifact_stem(&f.attempt.run_dir, "test");
    assert_eq!(std::fs::read_to_string(session.join("logs").join(format!("{stem}.log"))).unwrap(), "failure details");
    assert_eq!(std::fs::read_to_string(session.join("casts").join(format!("{stem}.cast"))).unwrap(), "recording");
    assert_eq!(std::fs::read_to_string(session.join("results").join(stem).join("tmp/result.json")).unwrap(), "result");
  }

  #[test]
  fn recovery_waits_for_host_consumers_and_adopts_a_dead_owner() {
    let mut f = Fixture::new();
    f.write(runtime::RUN_LOG_REL, "saved after interruption");
    f.attempt.save().unwrap();
    let mut legacy = crate::daemon::prune::PruneQueue::default();
    assert!(legacy.schedule(
      &f.attempt.run_dir.to_string_lossy(),
      f.attempt.run_dir.file_name().unwrap().to_str().unwrap(),
      "docker",
      true,
      0
    ));
    assert_eq!(legacy.tick(1), 0, "an orphan event must not bypass the ownership record");
    recover_pending(Instant::now() + BUDGET);
    assert!(f.attempt.run_dir.exists(), "live host may still be integrating commits");
    f.attempt.owner_pid = 2_000_000_000;
    assert!(!crate::daemon::pid_alive(f.attempt.owner_pid));
    f.attempt.save().unwrap();
    recover_pending(Instant::now() + BUDGET);
    assert!(!f.attempt.run_dir.exists());
    assert!(!journal_path(&f.attempt.run_dir).exists());
  }

  #[cfg(unix)]
  #[test]
  fn unknown_inspection_preserves_mount_and_a_later_retry_removes_it() {
    let mut f = Fixture::new();
    f.attempt.container_started = true;
    f.attempt.save().unwrap();
    f.runtime_script(
      r#"#!/bin/sh
if [ "$1" = inspect ]; then
  echo 'cannot connect to socket: no such file or directory' >&2
  exit 1
fi
"#,
    );
    assert!(f.attempt.finish(Instant::now() + BUDGET).is_err());
    assert!(f.attempt.run_dir.exists());
    assert!(journal_path(&f.attempt.run_dir).exists());
    f.runtime_script(
      r#"#!/bin/sh
echo '[]'
"#,
    );
    f.attempt.finish(Instant::now() + BUDGET).unwrap();
    assert!(!f.attempt.run_dir.exists());
  }

  #[cfg(unix)]
  #[test]
  fn daemon_recovery_removes_a_container_that_survived_the_runner() {
    let mut f = Fixture::new();
    f.attempt.container_started = true;
    f.attempt.owner_pid = 2_000_000_000;
    f.attempt.save().unwrap();
    f.runtime_script(
      r#"#!/bin/sh
state="${0%/*}/removed"
case "$1" in
  inspect) if [ -f "$state" ]; then echo '[]'; else echo '[{"Id":"leftover"}]'; fi ;;
  rm) touch "$state" ;;
esac
"#,
    );
    recover_pending(Instant::now() + BUDGET);
    assert!(f.home.join("bin/removed").exists());
    assert!(!f.attempt.run_dir.exists());
    assert!(!journal_path(&f.attempt.run_dir).exists());
  }

  #[test]
  fn invalid_ownership_and_expired_deadline_never_delete_scratch() {
    let f = Fixture::new();
    f.attempt.save().unwrap();
    assert!(f.attempt.finish(Instant::now()).is_err());
    assert!(f.attempt.run_dir.is_dir());
    let mut invalid = f.attempt.clone();
    invalid.outputs.push("../outside".into());
    assert!(invalid.save().is_err());
    invalid = f.attempt.clone();
    invalid.run_dir = PathBuf::from("/Users/scsh-abcdef-run-foreign");
    assert!(invalid.save().is_err());
    assert!(parse("{}").is_err());
  }

  #[test]
  fn commit_recovery_bundle_survives_removing_the_clone() {
    let mut f = Fixture::new();
    let git = |args: &[&str]| {
      let out = crate::git_command()
        .arg("-C")
        .arg(&f.attempt.run_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.org")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.org")
        .args(args)
        .output()
        .unwrap();
      assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
      String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["commit", "--allow-empty", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["commit", "--allow-empty", "-qm", "new work"]);
    let tip = git(&["rev-parse", "HEAD"]);
    f.attempt.commit_base = Some(base);
    f.attempt.save().unwrap();
    recover_pending(Instant::now() + BUDGET);
    assert!(f.attempt.run_dir.join(".git").exists());
    f.attempt.finish(Instant::now() + BUDGET).unwrap();
    let bundle = runtime::session_results_dir(&f.attempt.session)
      .join(format!("{}.bundle", artifact_stem(&f.attempt.run_dir, "test")));
    let out = crate::git_command().args(["bundle", "list-heads"]).arg(bundle).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(&tip));
    assert!(!f.attempt.run_dir.exists());
  }
}
