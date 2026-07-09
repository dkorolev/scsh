//! The MIT-only dependency policy, as code.
//!
//! scsh's own code is MIT, and every dependency must be usable under plain MIT too: either
//! MIT outright, or a dual/multi license whose `OR` lets the consumer elect MIT (scsh does —
//! see LICENSE.md "Third-party licenses"). Apache-only material is what this repo just spent
//! effort removing (the vendored asciinema-player); the test below keeps a future
//! `cargo update` or new dependency from quietly reintroducing a license MIT cannot cover.

/// Whether a Cargo SPDX license expression lets a consumer take the crate under plain MIT:
/// one of its top-level `OR` alternatives (Cargo's legacy `/` separator reads as `OR`) must
/// be exactly `MIT`. `MIT AND Apache-2.0` therefore does NOT qualify — both apply at once —
/// and neither does an empty expression (`license-file`-only crates need a human look).
pub fn mit_choosable(expr: &str) -> bool {
  expr.replace('/', " OR ").split(" OR ").any(|alt| alt.trim() == "MIT")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mit_choosable_reads_spdx_or_alternatives() {
    assert!(mit_choosable("MIT"));
    assert!(mit_choosable("MIT OR Apache-2.0"));
    assert!(mit_choosable("Apache-2.0 OR MIT"));
    assert!(mit_choosable("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"));
    assert!(mit_choosable("Apache-2.0/MIT")); // legacy separator
    assert!(!mit_choosable("Apache-2.0"));
    assert!(!mit_choosable("MIT AND Apache-2.0")); // both apply — not electable
    assert!(!mit_choosable("MITigation-1.0")); // exact token, not a prefix
    assert!(!mit_choosable(""));
  }

  /// Every crate in the lockfile — dev, build, and target-specific deps included — must be
  /// usable under plain MIT. Runs `cargo metadata` (locked, no network beyond what a build
  /// already fetched) and checks each package's SPDX expression. If this fails, either pick
  /// a different crate or bring the question to a human; do NOT weaken `mit_choosable`.
  #[test]
  fn every_dependency_is_usable_under_plain_mit() {
    let out = std::process::Command::new(env!("CARGO"))
      .args(["metadata", "--format-version", "1", "--locked"])
      .current_dir(env!("CARGO_MANIFEST_DIR"))
      .output()
      .expect("cargo metadata runs");
    assert!(out.status.success(), "cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr));
    let doc = crate::json::parse(&String::from_utf8_lossy(&out.stdout)).expect("metadata is JSON");
    let crate::json::Value::Object(root) = doc else { panic!("metadata root is an object") };
    let packages = root.iter().find(|(k, _)| k == "packages").map(|(_, v)| v).expect("packages array");
    let crate::json::Value::Array(packages) = packages else { panic!("packages is an array") };
    assert!(packages.len() > 5, "suspiciously few packages — did metadata change shape?");
    let mut bad: Vec<String> = Vec::new();
    for p in packages {
      let crate::json::Value::Object(fields) = p else { continue };
      let get = |key: &str| {
        fields.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
          crate::json::Value::String(s) => Some(s.clone()),
          _ => None,
        })
      };
      let name = get("name").unwrap_or_default();
      let license = get("license").unwrap_or_default();
      if !mit_choosable(&license) {
        bad.push(format!("{name}: '{license}'"));
      }
    }
    assert!(bad.is_empty(), "dependencies not usable under plain MIT:\n  {}", bad.join("\n  "));
  }
}
