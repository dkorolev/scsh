//! scsh version and build-time git stamp for CLI and session browser UI.

use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/scsh_build_info.rs"));

/// Crate version from `Cargo.toml` (e.g. `1.8.0`).
pub fn pkg_version() -> &'static str {
  env!("CARGO_PKG_VERSION")
}

/// Git short hash stamped at build time by `build.rs`: `git describe` in a checkout
/// (with `-dirty` when the tree is not clean), or the release commit from the
/// `.cargo_vcs_info.json` that cargo embeds in every published crate — so a
/// `cargo install scsh` build reports the exact main commit it was cut from. Empty only
/// when neither source knows the commit.
///
/// Deliberately NOT a runtime `git describe`: an installed binary must report one stable
/// identity, not borrow the git state of whatever directory it happens to run in. The old
/// runtime fallback made the same binary print different hashes — and a spurious `-dirty`
/// — depending on the caller's working tree.
pub fn git_stamp() -> String {
  static CACHE: OnceLock<String> = OnceLock::new();
  CACHE.get_or_init(|| GIT_DESCRIBE.to_string()).clone()
}

/// CLI-style version line: `1.8.0 (85555ff-dirty)` in a dev build, or just `1.8.0` for a
/// crates.io install.
pub fn display() -> String {
  let git = git_stamp();
  if git.is_empty() {
    pkg_version().to_string()
  } else {
    format!("{} ({git})", pkg_version())
  }
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
