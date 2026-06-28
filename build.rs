//! Build script: stamp the binary with the current git commit (seven hex digits) and
//! whether the tree was dirty at build time, so `scsh version` can report them.
//! Std-only — shells out to `git` with `-C $CARGO_MANIFEST_DIR` so the hash is found
//! even when cargo's working directory is not the crate root.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
  let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
  let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

  println!("cargo:rerun-if-changed=build.rs");
  println!("cargo:rerun-if-env-changed=SCSH_GIT_DESCRIBE");
  let head = manifest_dir.join(".git/HEAD");
  let index = manifest_dir.join(".git/index");
  if head.exists() {
    println!("cargo:rerun-if-changed={}", head.display());
  }
  if index.exists() {
    println!("cargo:rerun-if-changed={}", index.display());
  }

  let describe = git_describe(&manifest_dir);
  if describe.is_empty() {
    println!(
      "cargo:warning=scsh: could not determine git commit from {}; `scsh version` will omit a hash",
      manifest_dir.display()
    );
  }

  std::fs::write(out_dir.join("scsh_build_info.rs"), format!("pub const GIT_DESCRIBE: &str = {describe:?};\n"))
    .expect("write scsh_build_info.rs");

  // Kept for integration tests (`option_env!("SCSH_GIT_DESCRIBE")`).
  println!("cargo:rustc-env=SCSH_GIT_DESCRIBE={describe}");
}

/// Seven hex digits of HEAD, plus `-dirty` when the tree is not clean. Empty when git is
/// unavailable or the crate is not in a checkout (e.g. a crates.io source tarball).
fn git_describe(repo: &Path) -> String {
  if let Ok(v) = std::env::var("SCSH_GIT_DESCRIBE") {
    if !v.is_empty() {
      return v;
    }
  }

  let hash: String = match git_in(repo, &["rev-parse", "HEAD"]) {
    Some(h) => h.chars().take(7).collect(),
    None => return String::new(),
  };

  if git_dirty(repo) {
    format!("{hash}-dirty")
  } else {
    hash
  }
}

fn git_in(repo: &Path, args: &[&str]) -> Option<String> {
  let out = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
  if !out.status.success() {
    return None;
  }
  let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
  (!s.is_empty()).then_some(s)
}

fn git_dirty(repo: &Path) -> bool {
  Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(["status", "--porcelain"])
    .output()
    .ok()
    .filter(|o| o.status.success())
    .is_some_and(|o| !o.stdout.is_empty())
}
