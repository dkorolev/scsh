//! scsh version and build-time git stamp for CLI and session browser UI.

include!(concat!(env!("OUT_DIR"), "/scsh_build_info.rs"));

/// Crate version from `Cargo.toml` (e.g. `1.8.0`).
pub fn pkg_version() -> &'static str {
  env!("CARGO_PKG_VERSION")
}

/// Git short hash from build time, or a runtime `git rev-parse` fallback; empty when unknown.
pub fn git_stamp() -> String {
  let embedded = GIT_DESCRIBE;
  if !embedded.is_empty() {
    return embedded.to_string();
  }
  runtime_git_describe().unwrap_or_default()
}

/// CLI-style version line: `1.8.0 (85555ff-dirty)` or just `1.8.0`.
pub fn display() -> String {
  let git = git_stamp();
  if git.is_empty() {
    pkg_version().to_string()
  } else {
    format!("{} ({git})", pkg_version())
  }
}

fn runtime_git_describe() -> Option<String> {
  let mut dir = std::env::current_dir().ok()?;
  for _ in 0..32 {
    if dir.join(".git").exists() {
      return runtime_git_describe_in(&dir);
    }
    dir = dir.parent()?.to_path_buf();
  }
  None
}

fn runtime_git_describe_in(repo: &std::path::Path) -> Option<String> {
  let out = std::process::Command::new("git").arg("-C").arg(repo).args(["rev-parse", "HEAD"]).output().ok()?;
  if !out.status.success() {
    return None;
  }
  let hash: String = String::from_utf8_lossy(&out.stdout).trim().chars().take(7).collect();
  if hash.is_empty() {
    return None;
  }
  let dirty = std::process::Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(["status", "--porcelain"])
    .output()
    .ok()
    .filter(|o| o.status.success())
    .is_some_and(|o| !o.stdout.is_empty());
  Some(if dirty { format!("{hash}-dirty") } else { hash })
}

#[cfg(test)]
mod tests {
  use super::{display, pkg_version};

  #[test]
  fn display_includes_pkg_version() {
    let line = display();
    assert!(line.starts_with(pkg_version()));
  }
}
