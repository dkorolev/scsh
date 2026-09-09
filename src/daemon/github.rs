//! Host-side preparation of a GitHub pull request for the browser review flow.
//!
//! This deliberately runs on the daemon host: an ordinary skill container cannot create a
//! second scsh fleet. The resulting checkout is an scsh-owned, clean repository that the normal
//! job starter can hand to the globally installed `code-gorgeous-review` profile.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::json::{parse, quote, Value};

const NOTES_NAME: &str = "Elon Presley";
const NOTES_EMAIL: &str = "dmitry.korolev+elon-presley@gmail.com";
const OWNERSHIP_MARKER: &str = "scsh-github-review";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestRef {
  pub owner: String,
  pub repo: String,
  pub number: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
  pub reference: PullRequestRef,
  pub title: String,
  pub body: String,
  pub base_ref: String,
  /// Exact base revision reported by GitHub, including the historical base of a merged PR.
  pub base_oid: String,
  /// Exact reviewed head; preparation refuses a PR ref that moves after metadata was loaded.
  pub head_oid: String,
  pub url: String,
}

/// Durable context attached to a browser-started review while its fleet process runs.
#[derive(Clone, Debug)]
pub struct BrowserReview {
  pub pull_request: PullRequest,
}

pub fn parse_pull_request(input: &str) -> Result<PullRequestRef, String> {
  let input = input.trim().trim_end_matches('/');
  let parts = if let Some(rest) = input.strip_prefix("https://github.com/") {
    let fields: Vec<_> = rest.split('/').collect();
    if fields.len() != 4 || fields[2] != "pull" {
      return Err("use a GitHub pull-request URL such as https://github.com/owner/repo/pull/123".into());
    }
    (fields[0], fields[1], fields[3])
  } else if let Some((repo, number)) = input.split_once('#') {
    let Some((owner, name)) = repo.split_once('/') else {
      return Err("use owner/repo#123 or a full GitHub pull-request URL".into());
    };
    (owner, name, number)
  } else {
    return Err("use owner/repo#123 or a full GitHub pull-request URL".into());
  };
  let valid_slug = |s: &str| {
    !s.is_empty()
      && s != "."
      && s != ".."
      && s.len() <= 100
      && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
  };
  if !valid_slug(parts.0) || !valid_slug(parts.1) {
    return Err("the GitHub owner and repository contain unsupported characters".into());
  }
  let number = parts.2.parse::<u64>().map_err(|_| "the pull-request number must be a positive integer".to_string())?;
  if number == 0 {
    return Err("the pull-request number must be a positive integer".into());
  }
  Ok(PullRequestRef { owner: parts.0.into(), repo: parts.1.into(), number })
}

pub fn load_pull_request(gh: &Path, reference: PullRequestRef) -> Result<PullRequest, String> {
  let url = format!("https://github.com/{}/{}/pull/{}", reference.owner, reference.repo, reference.number);
  let output = Command::new(gh)
    .args(["pr", "view", &url, "--json", "title,body,baseRefName,baseRefOid,headRefOid,url"])
    .output()
    .map_err(|e| format!("could not run gh: {e}"))?;
  let stdout = checked_output(output, "gh pr view")?;
  let Value::Object(obj) = parse(&stdout).map_err(|e| format!("gh returned invalid pull-request metadata: {e}"))?
  else {
    return Err("gh returned invalid pull-request metadata".into());
  };
  let get = |name: &str| field_str(&obj, name).ok_or_else(|| format!("gh metadata omitted '{name}'"));
  Ok(PullRequest {
    reference,
    title: get("title")?,
    body: field_str(&obj, "body").unwrap_or_default(),
    base_ref: get("baseRefName")?,
    base_oid: get("baseRefOid")?,
    head_oid: get("headRefOid")?,
    url: get("url")?,
  })
}

fn field_str(obj: &[(String, Value)], name: &str) -> Option<String> {
  obj.iter().find(|(key, _)| key == name).and_then(|(_, value)| match value {
    Value::String(value) => Some(value.clone()),
    _ => None,
  })
}

pub fn prepare_pull_request(gh: &Path, home: &Path, pr: &PullRequest) -> Result<PathBuf, String> {
  for oid in [&pr.base_oid, &pr.head_oid] {
    if oid.len() != 40 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
      return Err("GitHub returned an invalid PR revision".into());
    }
  }
  let root = home.join("github-reviews");
  std::fs::create_dir_all(&root).map_err(|e| format!("could not create {}: {e}", root.display()))?;
  let dir = root.join(format!("pr-{}-{}-{}", pr.reference.number, pr.reference.repo, pr.reference.owner));
  let canonical = format!("{}/{}", pr.reference.owner, pr.reference.repo);
  let existed = dir.exists();
  let previous_head = if existed {
    verify_owned_checkout(&dir, &canonical)?
  } else {
    let output = Command::new(gh)
      .args(["repo", "clone", &canonical])
      .arg(&dir)
      .output()
      .map_err(|e| format!("could not run gh repo clone: {e}"))?;
    if let Err(e) = checked_output(output, "gh repo clone") {
      let _ = std::fs::remove_dir_all(&dir);
      return Err(e);
    }
    let marker = dir.join(".git").join(OWNERSHIP_MARKER);
    std::fs::write(&marker, format!("{canonical}\n")).map_err(|e| format!("could not mark {}: {e}", dir.display()))?;
    None
  };

  git(&dir, &["fetch", "origin"])?;
  git(&dir, &["fetch", "origin", &format!("pull/{}/head", pr.reference.number)])?;
  if existed {
    ensure_refresh_is_safe(&dir, previous_head.as_deref())?;
  }
  let imported_head = git_capture(&dir, &["rev-parse", "FETCH_HEAD"])?;
  if imported_head.trim() != pr.head_oid {
    return Err("the PR head changed during preparation; start the review again".into());
  }
  let branch = format!("pr-{}-{}-{}", pr.reference.number, pr.reference.repo, pr.reference.owner);
  git(&dir, &["checkout", "-B", &branch, "FETCH_HEAD"])?;
  git(&dir, &["clean", "-fd"])?;
  git(&dir, &["fetch", "origin", &pr.base_oid])?;
  git(&dir, &["branch", "-f", "main", &pr.base_oid])?;

  let exclude = dir.join(".git/info/exclude");
  let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
  if !existing.lines().any(|line| line.trim() == "/tmp/") {
    let separator = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    std::fs::write(&exclude, format!("{existing}{separator}/tmp/\n"))
      .map_err(|e| format!("could not update {}: {e}", exclude.display()))?;
  }
  std::fs::create_dir_all(dir.join("tmp")).map_err(|e| format!("could not create tmp/: {e}"))?;

  let body = if pr.body.is_empty() { "(The pull request has no description.)" } else { &pr.body };
  let notes = format!("# {}\n\n{}\n\n> Reconstructed from {} for local review.\n", pr.title, body, pr.url);
  std::fs::write(dir.join("PR-DESCRIPTION.md"), notes)
    .map_err(|e| format!("could not write PR-DESCRIPTION.md: {e}"))?;
  // This generated review input belongs only to our owned replica, even when the
  // upstream repository or global excludes intentionally ignore the filename.
  git(&dir, &["add", "-f", "--", "PR-DESCRIPTION.md"])?;
  git_env(
    &dir,
    &[
      "-c",
      &format!("user.name={NOTES_NAME}"),
      "-c",
      &format!("user.email={NOTES_EMAIL}"),
      "commit",
      "-qm",
      "Add PR-DESCRIPTION.md",
      "--",
      "PR-DESCRIPTION.md",
    ],
    &[("GIT_COMMITTER_NAME", NOTES_NAME), ("GIT_COMMITTER_EMAIL", NOTES_EMAIL)],
  )?;
  std::fs::write(dir.join(".git").join(OWNERSHIP_MARKER), format!("{canonical}\n{}\n", imported_head.trim()))
    .map_err(|e| format!("could not update the review ownership marker: {e}"))?;
  Ok(dir)
}

/// Receipt through which a later `$gh-gorgeous-review` invocation recognizes browser work and
/// resumes at report/publication rather than launching the expensive fleet a second time.
pub fn write_browser_receipt(root: &Path, pr: &PullRequest, session: &str, state: &str) {
  let reviewed_head = git_capture(root, &["rev-parse", "HEAD^"]).unwrap_or_default();
  let base_head = git_capture(root, &["rev-parse", "main"]).unwrap_or_default();
  let body = format!(
    "{{\"operation\":\"gh-gorgeous-review\",\"url\":{},\"session\":{},\"state\":{},\"reviewed_head\":{},\"base_ref\":{},\"base_head\":{}}}\n",
    quote(&pr.url),
    quote(session),
    quote(state),
    quote(reviewed_head.trim()),
    quote(&pr.base_ref),
    quote(base_head.trim()),
  );
  let _ = std::fs::create_dir_all(root.join("tmp"));
  let _ = std::fs::write(root.join("tmp/gh-gorgeous-review-browser.json"), body);
}

fn verify_owned_checkout(dir: &Path, canonical: &str) -> Result<Option<String>, String> {
  let marker = dir.join(".git").join(OWNERSHIP_MARKER);
  let owner = std::fs::read_to_string(&marker).unwrap_or_default();
  let mut lines = owner.lines();
  if lines.next() != Some(canonical) {
    return Err(format!(
      "{} already exists but is not an scsh-owned replica of {canonical}; move it aside once, then retry",
      dir.display()
    ));
  }
  Ok(lines.next().filter(|line| !line.is_empty()).map(str::to_string))
}

fn ensure_refresh_is_safe(dir: &Path, previous_head: Option<&str>) -> Result<(), String> {
  let status = git_capture(dir, &["status", "--porcelain", "--untracked-files=all"])?;
  if !status.trim().is_empty() {
    return Err(format!(
      "the saved review checkout has local changes; preserve or remove them before retrying:\n{status}"
    ));
  }
  if let Some(previous_head) = previous_head {
    let parent = git_capture(dir, &["rev-parse", "HEAD^"])?;
    let author = git_capture(dir, &["show", "-s", "--format=%an <%ae>", "HEAD"])?;
    let paths = git_capture(dir, &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])?;
    if parent.trim() != previous_head
      || author.trim() != format!("{NOTES_NAME} <{NOTES_EMAIL}>")
      || paths.lines().any(|path| path != "PR-DESCRIPTION.md")
    {
      return Err(
        "the saved review checkout no longer matches scsh's last imported PR head; preserve or remove it before retrying"
          .into(),
      );
    }
    return Ok(());
  }
  let local = git_capture(dir, &["rev-list", "FETCH_HEAD..HEAD"])?;
  for commit in local.lines().filter(|line| !line.is_empty()) {
    let author = git_capture(dir, &["show", "-s", "--format=%an <%ae>", commit])?;
    let paths = git_capture(dir, &["diff-tree", "--no-commit-id", "--name-only", "-r", commit])?;
    if author.trim() != format!("{NOTES_NAME} <{NOTES_EMAIL}>") || paths.lines().any(|p| p != "PR-DESCRIPTION.md") {
      return Err(
        "the saved review checkout has local commits not created by scsh; preserve or remove them before retrying"
          .into(),
      );
    }
  }
  Ok(())
}

fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
  git_env(dir, args, &[])
}

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<(), String> {
  let mut cmd = crate::git_command();
  cmd.arg("-C").arg(dir).args(args);
  for (name, value) in env {
    cmd.env(name, value);
  }
  let output = cmd.output().map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
  checked_output(output, &format!("git {}", args.first().unwrap_or(&""))).map(|_| ())
}

fn git_capture(dir: &Path, args: &[&str]) -> Result<String, String> {
  let output = crate::git_command()
    .arg("-C")
    .arg(dir)
    .args(args)
    .output()
    .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
  checked_output(output, &format!("git {}", args.first().unwrap_or(&"")))
}

fn checked_output(output: Output, context: &str) -> Result<String, String> {
  if output.status.success() {
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
  } else {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() { stdout.trim() } else { stderr.trim() };
    Err(format!("{context} failed: {detail}"))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("scsh-github-{tag}-{}", crate::runtime::random_nonce_6()))
  }

  #[test]
  fn parses_full_and_shorthand_pull_request_references() {
    let expected = PullRequestRef { owner: "dkorolev".into(), repo: "scsh".into(), number: 42 };
    assert_eq!(parse_pull_request("https://github.com/dkorolev/scsh/pull/42/").unwrap(), expected);
    assert_eq!(parse_pull_request("dkorolev/scsh#42").unwrap(), expected);
  }

  #[test]
  fn rejects_non_pull_urls_and_unsafe_path_components() {
    for bad in [
      "https://github.com/dkorolev/scsh/issues/42",
      "https://example.com/dkorolev/scsh/pull/42",
      "../scsh#42",
      "dkorolev/scsh#0",
    ] {
      assert!(parse_pull_request(bad).is_err(), "accepted {bad}");
    }
  }

  #[cfg(unix)]
  #[test]
  fn prepares_and_safely_refreshes_a_review_checkout() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = temp_dir("prepare");
    let source = fixture.join("source");
    let remote = fixture.join("remote.git");
    let home = fixture.join("home");
    std::fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "-q", "-b", "main"]).unwrap();
    std::fs::write(source.join("base.txt"), "base\n").unwrap();
    std::fs::write(source.join(".gitignore"), "/PR-DESCRIPTION.md\n").unwrap();
    git(&source, &["add", "base.txt", ".gitignore"]).unwrap();
    git_env(
      &source,
      &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.com", "commit", "-qm", "Base."],
      &[],
    )
    .unwrap();
    let base = git_capture(&source, &["rev-parse", "HEAD"]).unwrap();
    git(&source, &["checkout", "-qb", "feature"]).unwrap();
    std::fs::write(source.join("feature.txt"), "feature\n").unwrap();
    git(&source, &["add", "feature.txt"]).unwrap();
    git_env(
      &source,
      &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.com", "commit", "-qm", "Feature."],
      &[],
    )
    .unwrap();
    let head = git_capture(&source, &["rev-parse", "HEAD"]).unwrap();
    let output = crate::git_command().args(["clone", "--bare"]).arg(&source).arg(&remote).output().unwrap();
    checked_output(output, "git clone --bare").unwrap();
    git(&remote, &["update-ref", "refs/pull/7/head", head.trim()]).unwrap();

    let gh = fixture.join("gh");
    std::fs::write(
      &gh,
      format!(
        "#!/bin/sh\nif [ \"$1\" = repo ] && [ \"$2\" = clone ]; then exec git clone '{}' \"$4\"; fi\nexit 2\n",
        remote.display()
      ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut pr = PullRequest {
      reference: PullRequestRef { owner: "owner".into(), repo: "repo".into(), number: 7 },
      title: "A useful change".into(),
      body: "The real body.".into(),
      base_ref: "main".into(),
      base_oid: base.trim().into(),
      head_oid: head.trim().into(),
      url: "https://github.com/owner/repo/pull/7".into(),
    };

    // Model a merged PR: the branch tip already contains the feature, but GitHub's
    // recorded base is still the pre-merge commit. Review that exact historical diff.
    git(&remote, &["update-ref", "refs/heads/main", head.trim()]).unwrap();
    let checkout = prepare_pull_request(&gh, &home, &pr).unwrap();
    assert_eq!(git_capture(&checkout, &["rev-parse", "main"]).unwrap().trim(), base.trim());
    assert_eq!(git_capture(&checkout, &["rev-parse", "HEAD^"]).unwrap().trim(), head.trim());
    assert_eq!(
      git_capture(&checkout, &["show", "-s", "--format=%an <%ae>"]).unwrap().trim(),
      format!("{NOTES_NAME} <{NOTES_EMAIL}>")
    );
    assert!(std::fs::read_to_string(checkout.join("PR-DESCRIPTION.md")).unwrap().contains("The real body."));
    assert_eq!(git_capture(&checkout, &["show", "HEAD:.gitignore"]).unwrap(), "/PR-DESCRIPTION.md\n");
    assert!(git_capture(&checkout, &["status", "--porcelain"]).unwrap().is_empty());
    write_browser_receipt(&checkout, &pr, "abcxyz", "running");
    let receipt = std::fs::read_to_string(checkout.join("tmp/gh-gorgeous-review-browser.json")).unwrap();
    assert!(receipt.contains(r#""operation":"gh-gorgeous-review""#));
    assert!(receipt.contains(r#""session":"abcxyz""#));
    assert!(receipt.contains(&format!(r#""reviewed_head":"{}""#, head.trim())));

    git(&source, &["checkout", "-q", "main"]).unwrap();
    git(&source, &["checkout", "-qb", "rewritten-feature"]).unwrap();
    std::fs::write(source.join("replacement.txt"), "replacement\n").unwrap();
    git(&source, &["add", "replacement.txt"]).unwrap();
    git_env(
      &source,
      &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.com", "commit", "-qm", "Replacement feature."],
      &[],
    )
    .unwrap();
    let rewritten_head = git_capture(&source, &["rev-parse", "HEAD"]).unwrap();
    let pushed = crate::git_command()
      .arg("-C")
      .arg(&source)
      .args(["push", "--force"])
      .arg(&remote)
      .arg("HEAD:refs/pull/7/head")
      .output()
      .unwrap();
    checked_output(pushed, "git push rewritten PR").unwrap();

    assert!(prepare_pull_request(&gh, &home, &pr).unwrap_err().contains("head changed"));
    pr.head_oid = rewritten_head.trim().into();
    let refreshed = prepare_pull_request(&gh, &home, &pr).unwrap();
    assert_eq!(refreshed, checkout);
    assert_eq!(git_capture(&checkout, &["rev-parse", "HEAD^"]).unwrap().trim(), rewritten_head.trim());
    assert!(git_capture(&checkout, &["status", "--porcelain"]).unwrap().is_empty());
    std::fs::remove_dir_all(&fixture).ok();
  }
}
