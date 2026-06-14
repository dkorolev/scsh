//! Build script: stamp the binary with the current git commit (its first seven hex
//! digits) and whether the tree was dirty at build time, so `scsh version` can report
//! them. Std-only — it shells out to `git` and adds no crates, so the binary keeps its
//! zero runtime dependencies. The value is exposed as the `SCSH_GIT_DESCRIBE` env var,
//! read in `main.rs` via `env!`: empty when built outside a git checkout, otherwise
//! `abc1234` (clean) or `abc1234-dirty`.

use std::process::Command;

fn main() {
  // Re-run when the commit or the staged index changes (covers `git commit` / `git
  // add`). A pure *unstaged* edit after a build won't retrigger this until the next
  // rebuild — the stamp reflects the tree as of the last build, which is what we want.
  println!("cargo:rerun-if-changed=.git/HEAD");
  println!("cargo:rerun-if-changed=.git/index");

  // The first seven hex digits of HEAD (exactly seven — not git's variable --short).
  let hash = git(&["rev-parse", "HEAD"]).map(|h| h.chars().take(7).collect::<String>()).unwrap_or_default();
  // Dirty = anything `git status --porcelain` reports (gitignored paths excluded).
  let dirty = match Command::new("git").args(["status", "--porcelain"]).output() {
    Ok(out) => out.status.success() && !out.stdout.is_empty(),
    Err(_) => false,
  };

  let describe = if hash.is_empty() {
    String::new()
  } else if dirty {
    format!("{hash}-dirty")
  } else {
    hash
  };
  println!("cargo:rustc-env=SCSH_GIT_DESCRIBE={describe}");
}

/// Run `git <args>` and return its trimmed stdout, or `None` on any failure.
fn git(args: &[&str]) -> Option<String> {
  let out = Command::new("git").args(args).output().ok()?;
  if !out.status.success() {
    return None;
  }
  let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
  if s.is_empty() {
    None
  } else {
    Some(s)
  }
}
