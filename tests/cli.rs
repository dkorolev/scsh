//! End-to-end tests that drive the compiled `scsh` binary.
//!
//! These require `git` and a container runtime (docker/podman) on PATH, which
//! the preflight checks for. They never pull an image or build a container:
//! every case stops at `list` (or an earlier guard), so no network is touched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn bin() -> &'static str {
  env!("CARGO_BIN_EXE_scsh")
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_dir(tag: &str) -> PathBuf {
  let n = COUNTER.fetch_add(1, Ordering::Relaxed);
  let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
  let mut p = std::env::temp_dir();
  p.push(format!("scsh-it-{tag}-{}-{nanos}-{n}", std::process::id()));
  std::fs::create_dir_all(&p).unwrap();
  p
}

fn git_init(dir: &Path) {
  let ok = Command::new("git").args(["init", "-q", "."]).current_dir(dir).status().expect("run git init").success();
  assert!(ok, "git init should succeed in {}", dir.display());
  // A local identity so `--init-demo-project`'s auto-commit is deterministic in tests
  // (independent of any global git config on the machine running them).
  git(dir, &["config", "user.email", "scsh-test@example.com"]);
  git(dir, &["config", "user.name", "scsh test"]);
}

fn git(dir: &Path, args: &[&str]) {
  let ok = Command::new("git").args(args).current_dir(dir).status().expect("run git").success();
  assert!(ok, "git {args:?} should succeed in {}", dir.display());
}

fn git_clean(dir: &Path) -> bool {
  let out = Command::new("git").args(["status", "--porcelain"]).current_dir(dir).output().expect("git status");
  out.stdout.is_empty()
}

struct Run {
  code: i32,
  out: String,
}

fn scsh(dir: &Path, args: &[&str]) -> Run {
  let output = Command::new(bin()).args(args).current_dir(dir).output().expect("run scsh");
  let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
  out.push_str(&String::from_utf8_lossy(&output.stderr));
  Run { code: output.status.code().unwrap_or(-1), out }
}

/// The version scsh displays (crate semver from `CARGO_PKG_VERSION`).
fn shown_version() -> &'static str {
  env!("CARGO_PKG_VERSION")
}

#[test]
fn version_prints_version() {
  let d = unique_dir("version");
  let r = scsh(&d, &["--version"]);
  assert_eq!(r.code, 0);
  assert!(r.out.contains(&format!("scsh {}", shown_version())), "got: {}", r.out);
}

#[test]
fn help_describes_the_tool() {
  let d = unique_dir("help");
  let r = scsh(&d, &["--help"]);
  assert_eq!(r.code, 0);
  assert!(r.out.contains("Scoped Skills Helper"));
  assert!(r.out.contains("--init-demo-project")); // the aliases footer
  assert!(r.out.contains("Commands:"));
  // The long detail is deliberately NOT on the default page — it lives in the topics.
  assert!(!r.out.contains("Preflight order"), "overview must stay compact; got: {}", r.out);
}

#[test]
fn help_topics_are_separate_pages() {
  let d = unique_dir("helptopics"); // not a git repo — help never preflights
                                    // `scsh help .scsh.yml` is the config page: fields + the env syntax.
  let cfg = scsh(&d, &["help", ".scsh.yml"]);
  assert_eq!(cfg.code, 0, "got: {}", cfg.out);
  assert!(
    cfg.out.contains("skills:") && cfg.out.contains("harness") && cfg.out.contains("Env value syntax"),
    "got: {}",
    cfg.out
  );
  // `scsh help run` is the agent-oriented run guide: profiles, preflight, exit codes.
  let run = scsh(&d, &["help", "run"]);
  assert_eq!(run.code, 0, "got: {}", run.out);
  assert!(
    run.out.contains("Synopsis") && run.out.contains("scsh list --json") && run.out.contains("Exit codes"),
    "got: {}",
    run.out
  );
  // `scsh help internals` is the how-it-works page: preflight order + the clone/run.
  let internals = scsh(&d, &["help", "internals"]);
  assert_eq!(internals.code, 0, "got: {}", internals.out);
  assert!(internals.out.contains("Preflight order") && internals.out.contains("clone"), "got: {}", internals.out);
  // `scsh help cache` explains the (non-)caching model.
  let cache = scsh(&d, &["help", "cache"]);
  assert_eq!(cache.code, 0, "got: {}", cache.out);
  assert!(
    cache.out.contains("cache key") && cache.out.contains("tmp/.sccache") && cache.out.contains("(cached"),
    "got: {}",
    cache.out
  );
  // The overview points at all topics but does not carry their detail.
  let overview = scsh(&d, &["help"]);
  assert!(
    overview.out.contains("scsh help run")
      && overview.out.contains("scsh help .scsh.yml")
      && overview.out.contains("scsh help internals")
      && overview.out.contains("scsh help cache"),
    "got: {}",
    overview.out
  );
  // A mistyped topic is rejected, listing the valid ones.
  let bad = scsh(&d, &["help", "nope"]);
  assert_eq!(bad.code, 2, "got: {}", bad.out);
  assert!(bad.out.contains("unknown help topic"), "got: {}", bad.out);
}

#[test]
fn unknown_option_is_rejected() {
  let d = unique_dir("badopt");
  let r = scsh(&d, &["--nope"]);
  assert_eq!(r.code, 2);
  assert!(r.out.contains("unknown command or option"));
}

#[test]
fn no_command_shows_help() {
  // The default command is `help`: a bare `scsh` is safe and self-explanatory — it
  // shows help, it does NOT preflight or run (even outside a git repo).
  let d = unique_dir("nocmd"); // not a git repo
  let r = scsh(&d, &[]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("Scoped Skills Helper") && r.out.contains("Commands:"), "got: {}", r.out);
  assert!(!r.out.contains("not inside a git repository"), "bare scsh must not run; got: {}", r.out);
}

#[test]
fn subcommands_and_flag_aliases_agree() {
  // `help`/`version` work as subcommands; `list` and its `ls` alias behave the same (here:
  // both report the missing config). Rust-style commands + legacy flags.
  let d = unique_dir("subcmd");
  git_init(&d);
  assert_eq!(scsh(&d, &["help"]).code, 0);
  assert!(scsh(&d, &["version"]).out.contains(&format!("scsh {}", shown_version())));
  assert!(scsh(&d, &["list"]).out.contains(".scsh.yml not found"));
  assert!(scsh(&d, &["ls"]).out.contains(".scsh.yml not found"));
}

#[test]
fn help_covers_every_command_and_exit_codes() {
  let d = unique_dir("help");
  git_init(&d);
  // `help <command>` works for every command (and via alias), exit 0 with its synopsis.
  for (topic, needle) in [
    ("stats", "scsh stats"),
    ("annotate-cast", "scsh annotate-cast"),
    ("daemon", "scsh daemon"),
    ("ls", "scsh list"),                // alias resolves to `list`
    ("annotate", "scsh annotate-cast"), // alias resolves to `annotate-cast`
    ("export-cast", "scsh export-cast"),
    ("export", "scsh export-cast"), // alias resolves to `export-cast`
  ] {
    let r = scsh(&d, &["help", topic]);
    assert_eq!(r.code, 0, "help {topic} exit: {}", r.out);
    assert!(r.out.contains("Synopsis") && r.out.contains(needle), "help {topic} got: {}", r.out);
  }
  // `help exitcodes` documents the 0/1/2 table (§2 requires it).
  let ec = scsh(&d, &["help", "exitcodes"]);
  assert_eq!(ec.code, 0);
  assert!(ec.out.contains("Exit codes") && ec.out.contains("Usage error"), "got: {}", ec.out);
  // An unknown topic is a usage error that lists the real commands.
  let bad = scsh(&d, &["help", "nope"]);
  assert_eq!(bad.code, 2);
  assert!(bad.out.contains("unknown help topic 'nope'") && bad.out.contains("annotate-cast"), "got: {}", bad.out);
}

#[test]
fn version_reports_optional_git_describe() {
  // `scsh version` is `scsh <semver>` optionally followed by ` (<7 hex>[-dirty])` from
  // the build's git stamp. When this test crate was built from git, the hash must appear.
  let d = unique_dir("ver");
  let line = scsh(&d, &["version"]).out.lines().next().unwrap_or("").to_string();
  assert!(line.starts_with(&format!("scsh {}", shown_version())), "got: {line}");
  let embedded = option_env!("SCSH_GIT_DESCRIBE").filter(|s| !s.is_empty());
  if embedded.is_some() {
    assert!(line.contains(" ("), "expected git commit in version line; got: {line}");
  }
  if let Some(i) = line.find(" (") {
    let inner = line[i + 2..].trim_end_matches(')');
    let hex = inner.trim_end_matches("-dirty");
    assert_eq!(hex.len(), 7, "git short hash should be 7 hex digits; got '{hex}'");
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "non-hex in '{hex}'");
  }
}

#[test]
fn installskills_installs_skill_and_symlinks() {
  let d = unique_dir("install");
  git_init(&d);
  let r = scsh(&d, &["installskills"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  let p = d.join(".skills/scsh-harness-demo-and-selftest/SKILL.md");
  assert!(p.is_file(), "the bundled skill should be installed");
  assert!(std::fs::read_to_string(&p).unwrap().contains("name: scsh-harness-demo-and-selftest"));
  // With no URL, scsh nudges toward a real skills repo.
  assert!(r.out.contains("beautiful-skills"), "no-URL install should suggest a repo; got: {}", r.out);
  // The five harness discovery dirs are symlinks resolving to the skill.
  for host in [".claude/skills", ".codex/skills", ".cursor/skills", ".opencode/skills", ".agents/skills"] {
    let link = d.join(host);
    assert!(link.symlink_metadata().expect("symlink meta").file_type().is_symlink(), "{host} should be a symlink");
    assert!(link.join("scsh-harness-demo-and-selftest/SKILL.md").is_file(), "{host} should resolve to the skill");
  }
}

#[test]
fn installskills_is_idempotent_and_updateskills_overwrites() {
  let d = unique_dir("install2");
  git_init(&d);
  assert_eq!(scsh(&d, &["installskills"]).code, 0);
  // installskills now requires a clean tree, so commit the install before doing more.
  git(&d, &["add", "-A"]);
  git(&d, &["commit", "-qm", "install bundled skill"]);
  // Re-running with the same content is fine — already installed, not an error.
  let again = scsh(&d, &["installskills"]);
  assert_eq!(again.code, 0, "got: {}", again.out);
  assert!(again.out.contains("already installed"), "got: {}", again.out);

  // A locally-modified (and committed) skill must NOT be overwritten by installskills; it
  // suggests updateskills instead.
  let p = d.join(".skills/scsh-harness-demo-and-selftest/SKILL.md");
  std::fs::write(&p, "name: scsh-harness-demo-and-selftest\nMINE — do not touch\n").unwrap();
  git(&d, &["add", "-A"]);
  git(&d, &["commit", "-qm", "customize the skill"]);
  let kept = scsh(&d, &["installskills"]);
  assert_eq!(kept.code, 0, "got: {}", kept.out);
  assert!(kept.out.contains("updateskills"), "installskills should suggest updateskills; got: {}", kept.out);
  assert_eq!(std::fs::read_to_string(&p).unwrap(), "name: scsh-harness-demo-and-selftest\nMINE — do not touch\n");

  // updateskills overwrites it back to the bundled skill.
  let upd = scsh(&d, &["updateskills"]);
  assert_eq!(upd.code, 0, "got: {}", upd.out);
  assert!(
    std::fs::read_to_string(&p).unwrap().contains("name: scsh-harness-demo-and-selftest"),
    "updateskills should restore the bundled skill"
  );
}

#[test]
fn installskills_from_a_git_repo() {
  // A tiny "source" repo that ships one skill (with a script, to prove full-dir copy).
  let src = unique_dir("skillsrc");
  git_init(&src);
  std::fs::create_dir_all(src.join(".skills/foo/scripts")).unwrap();
  std::fs::write(src.join(".skills/foo/SKILL.md"), "name: foo\nthe foo skill\n").unwrap();
  std::fs::write(src.join(".skills/foo/scripts/run.sh"), "echo hi\n").unwrap();
  git(&src, &["add", "-A"]);
  git(&src, &["commit", "-qm", "ship foo"]);

  // Install it into a fresh target repo by URL/path.
  let dst = unique_dir("skilldst");
  git_init(&dst);
  let r = scsh(&dst, &["installskills", &src.to_string_lossy()]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(dst.join(".skills/foo/SKILL.md").is_file(), "the skill should be installed; got: {}", r.out);
  assert!(dst.join(".skills/foo/scripts/run.sh").is_file(), "skill scripts too; got: {}", r.out);
  assert!(r.out.contains("foo"), "should name the installed skill; got: {}", r.out);
  // The harness discovery symlinks are wired, exactly as for bundled installs.
  let link = dst.join(".claude/skills");
  assert!(link.symlink_metadata().expect("symlink meta").file_type().is_symlink());
  assert!(link.join("foo/SKILL.md").is_file(), "symlink should resolve to the skill");

  // installskills requires a clean tree, so commit the install before re-running.
  git(&dst, &["add", "-A"]);
  git(&dst, &["commit", "-qm", "install foo"]);
  // Re-running is idempotent: identical files are "already installed", not clobbered.
  let again = scsh(&dst, &["installskills", &src.to_string_lossy()]);
  assert_eq!(again.code, 0, "got: {}", again.out);
  assert!(again.out.contains("already installed"), "got: {}", again.out);
}

#[test]
fn installskills_copies_invocations_manifest_verbatim() {
  let src = unique_dir("fleetsrc");
  git_init(&src);
  std::fs::create_dir_all(src.join(".skills/reviewer")).unwrap();
  std::fs::write(src.join(".skills/reviewer/SKILL.md"), "name: reviewer\n").unwrap();
  std::fs::write(
    src.join(".scsh.yml"),
    r#"skills:
  reviewer:
    profile: code-review
    timeout: 600
    result: tmp/review-{name}.json
    invocations:
      opencode-gpt:
        harness: opencode
        model: openai/gpt-5.5
      claude-opus:
        harness: claude
        model: claude-opus-4-6
"#,
  )
  .unwrap();
  git(&src, &["add", "-A"]);
  git(&src, &["commit", "-qm", "ship fleet"]);

  let dst = unique_dir("fleetdst");
  git_init(&dst);
  let r = scsh(&dst, &["installskills", &src.to_string_lossy()]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(dst.join(".skills/reviewer/SKILL.md").is_file(), "skill folder copied; got: {}", r.out);
  assert!(r.out.contains("1 skill"), "one skill folder; got: {}", r.out);
  let cfg = std::fs::read_to_string(dst.join(".scsh.yml")).expect(".scsh.yml merged");
  assert!(cfg.contains("  reviewer:") && cfg.contains("invocations:"), "verbatim matrix block; got: {cfg}");
  assert!(cfg.contains("opencode-gpt:") && cfg.contains("claude-opus:"), "got: {cfg}");
}

#[test]
fn installskills_warns_on_manifest_key_conflict() {
  let src = unique_dir("conflictsrc");
  git_init(&src);
  std::fs::create_dir_all(src.join(".skills/foo")).unwrap();
  std::fs::write(src.join(".skills/foo/SKILL.md"), "name: foo\n").unwrap();
  std::fs::write(
    src.join(".scsh.yml"),
    r#"skills:
  foo:
    harness: opencode
    result: tmp/foo.json
"#,
  )
  .unwrap();
  git(&src, &["add", "-A"]);
  git(&src, &["commit", "-qm", "ship"]);

  let dst = unique_dir("conflictdst");
  git_init(&dst);
  std::fs::write(
    dst.join(".scsh.yml"),
    r#"skills:
  foo:
    harness: claude
    result: tmp/existing.json
"#,
  )
  .unwrap();
  git(&dst, &["add", "-A"]);
  git(&dst, &["commit", "-qm", "existing"]);

  let r = scsh(&dst, &["installskills", &src.to_string_lossy()]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("conflicts"), "got: {}", r.out);
  let cfg = std::fs::read_to_string(dst.join(".scsh.yml")).expect(".scsh.yml");
  assert!(cfg.contains("tmp/existing.json"), "consumer entry unchanged; got: {cfg}");
}

#[test]
fn installskills_refuses_a_dirty_tree() {
  // Like a real run, install insists on a clean tree so the install is a reviewable diff.
  let d = unique_dir("installdirty");
  git_init(&d);
  std::fs::write(d.join("WIP.txt"), "uncommitted work\n").unwrap();
  let r = scsh(&d, &["installskills"]);
  assert_eq!(r.code, 1, "should refuse on a dirty tree; got: {}", r.out);
  assert!(r.out.contains("uncommitted changes"), "got: {}", r.out);
  // Nothing was written on refusal — not the skills, not .gitignore.
  assert!(!d.join(".skills").exists(), "no skills on refusal; got: {}", r.out);
  assert!(!d.join(".gitignore").exists(), ".gitignore untouched on refusal; got: {}", r.out);
}

#[test]
fn installskills_makes_the_repo_run_ready() {
  // A clean install also ensures /tmp is gitignored, so the repo is run-ready afterward.
  let d = unique_dir("installtmp");
  git_init(&d);
  let r = scsh(&d, &["installskills"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("/tmp"), "should report adding /tmp to .gitignore; got: {}", r.out);
  let gi = std::fs::read_to_string(d.join(".gitignore")).expect(".gitignore written");
  assert!(gi.lines().any(|l| l.trim() == "/tmp"), "/tmp should be ignored; got: {}", gi);
}

#[test]
fn installskills_accepts_multiple_repos() {
  // Two source repos, each shipping one skill; installing both in one command installs each.
  let mk = |tag: &str, skill: &str| {
    let s = unique_dir(tag);
    git_init(&s);
    std::fs::create_dir_all(s.join(format!(".skills/{skill}"))).unwrap();
    std::fs::write(s.join(format!(".skills/{skill}/SKILL.md")), format!("name: {skill}\nthe {skill} skill\n")).unwrap();
    git(&s, &["add", "-A"]);
    git(&s, &["commit", "-qm", "ship"]);
    s
  };
  let s1 = mk("multi1", "alpha");
  let s2 = mk("multi2", "beta");
  let d = unique_dir("multidst");
  git_init(&d);
  let r = scsh(&d, &["installskills", &s1.to_string_lossy(), &s2.to_string_lossy()]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(d.join(".skills/alpha/SKILL.md").is_file(), "first repo's skill; got: {}", r.out);
  assert!(d.join(".skills/beta/SKILL.md").is_file(), "second repo's skill; got: {}", r.out);
  assert!(r.out.contains("alpha") && r.out.contains("beta"), "both named; got: {}", r.out);
}

#[test]
fn installskills_skips_and_reports_internal_skills() {
  // A source with no manifest: a normal skill installs; an `internal-` one is skipped AND named.
  let src = unique_dir("intsrc");
  git_init(&src);
  for name in ["normal", "internal-secret"] {
    std::fs::create_dir_all(src.join(format!(".skills/{name}"))).unwrap();
    std::fs::write(src.join(format!(".skills/{name}/SKILL.md")), format!("name: {name}\n")).unwrap();
  }
  git(&src, &["add", "-A"]);
  git(&src, &["commit", "-qm", "ship"]);
  let d = unique_dir("intdst");
  git_init(&d);
  let r = scsh(&d, &["installskills", &src.to_string_lossy()]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(d.join(".skills/normal/SKILL.md").is_file(), "normal skill installed; got: {}", r.out);
  assert!(!d.join(".skills/internal-secret").exists(), "internal-* not installed; got: {}", r.out);
  assert!(
    r.out.contains("internal-secret") && r.out.contains("authoring-only"),
    "the skip should be reported; got: {}",
    r.out
  );
}

#[test]
fn outside_git_repo_suggests_git_init() {
  let d = unique_dir("nogit"); // a bare temp dir is not a git repo
  let r = scsh(&d, &["run"]);
  assert_eq!(r.code, 1);
  assert!(r.out.contains("not inside a git repository"), "got: {}", r.out);
  assert!(r.out.contains("git init ."), "got: {}", r.out);
}

#[test]
fn missing_config_suggests_init_demo() {
  let d = unique_dir("nocfg");
  git_init(&d);
  let r = scsh(&d, &["list"]);
  assert_eq!(r.code, 1);
  assert!(r.out.contains(".scsh.yml not found"), "got: {}", r.out);
  assert!(r.out.contains("scsh init-demo-project"), "got: {}", r.out);
}

#[test]
fn init_demo_then_list() {
  let d = unique_dir("happy");
  git_init(&d);

  let init = scsh(&d, &["--init-demo-project"]);
  assert_eq!(init.code, 0, "got: {}", init.out);
  let cfg = std::fs::read_to_string(d.join(".scsh.yml")).expect(".scsh.yml written");
  // The v1.0 config is just the skills — no version/project/image boilerplate.
  assert!(
    cfg.contains("skills:") && cfg.contains("  add:") && cfg.contains("invocations:") && cfg.contains("  multiply:"),
    "got: {cfg}"
  );
  assert!(!cfg.contains("version:") && !cfg.contains("project:") && !cfg.contains("image:"), "got: {cfg}");

  // `scsh list`: every skill grouped by profile — `add` under `default`, `multiply` under
  // its profile, each with its result file. No container internals (those need --verbose).
  let list = scsh(&d, &["list"]);
  assert_eq!(list.code, 0, "got: {}", list.out);
  assert!(
    list.out.contains("add-opencode-gpt-5.4-mini-fast")
      && list.out.contains("tmp/add_opencode-gpt-5.4-mini-fast_result.json"),
    "got: {}",
    list.out
  );
  assert!(
    list.out.contains("multiply-opencode-gpt-5.4-mini-fast")
      && list.out.contains("tmp/multiply_opencode-gpt-5.4-mini-fast_result.json"),
    "got: {}",
    list.out
  );
  assert!(!list.out.contains("FROM debian"), "internals must be hidden without --verbose; got: {}", list.out);
  assert!(!list.out.contains("git clone"), "internals must be hidden without --verbose; got: {}", list.out);

  // `scsh list --verbose` reveals the Dockerfile and exact build/run commands.
  let v = scsh(&d, &["list", "--verbose"]);
  assert_eq!(v.code, 0, "got: {}", v.out);
  assert!(v.out.contains("FROM debian:bookworm-slim"), "got: {}", v.out);
  assert!(!v.out.contains("CMD ["), "image should bake no CMD; got: {}", v.out);
  assert!(v.out.contains("USER agent") && v.out.contains("AGENT_UID="), "got: {}", v.out);
  assert!(
    v.out.contains("scsh-opencode:latest")
      && (v.out.contains("git clone") || v.out.contains("transport.git"))
      && v.out.contains(":/home/agent"),
    "got: {}",
    v.out
  );
  // Every harness is a recorded interactive TUI pointed at the skill's SKILL.md (not the
  // old headless `opencode run "run skill …"` form). The container name still carries the
  // `-run-<invocation>` stem.
  assert!(
    v.out.contains("Run the skill defined in .skills/add/SKILL.md")
      && v.out.contains("-run-add-opencode-gpt-5.4-mini-fast"),
    "got: {}",
    v.out
  );
}

#[test]
fn list_groups_skills_by_profile() {
  // `scsh list` shows every skill under its profile: `add` under `default`, `multiply`
  // under `multiply`. `--profile` is run-only; `--verbose` is list-only. All network-free.
  let d = unique_dir("list");
  git_init(&d);
  assert_eq!(scsh(&d, &["--init-demo-project"]).code, 0);

  let list = scsh(&d, &["list"]);
  assert_eq!(list.code, 0, "got: {}", list.out);
  // Both skills appear with their result files, and the profile groups are shown.
  assert!(
    list.out.contains("tmp/add_opencode-gpt-5.4-mini-fast_result.json")
      && list.out.contains("tmp/multiply_opencode-gpt-5.4-mini-fast_result.json"),
    "got: {}",
    list.out
  );
  assert!(list.out.contains("default") && list.out.contains("multiply"), "got: {}", list.out);

  // Profiles only apply to `run` (parse-time rejection, exit 2).
  let p = scsh(&d, &["list", "--profile", "multiply"]);
  assert_eq!(p.code, 2, "got: {}", p.out);
  assert!(p.out.contains("profiles only apply to 'run'"), "got: {}", p.out);

  // `--verbose` only applies to `list` (parse-time rejection, exit 2).
  let v = scsh(&d, &["run", "--verbose"]);
  assert_eq!(v.code, 2, "got: {}", v.out);
  assert!(v.out.contains("--verbose only applies to 'list'"), "got: {}", v.out);
}

#[test]
fn list_shows_empty_default_profile() {
  // When every skill is profiled, `scsh list` shows the reserved `default` profile as empty
  // (a bare `scsh run` is a no-op) alongside the populated profiles, with counts.
  let d = unique_dir("emptydefault");
  git_init(&d);
  std::fs::write(d.join(".scsh.yml"), include_str!("fixtures/two-profile.scsh.yml")).unwrap();
  let r = scsh(&d, &["list"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("default (0)"), "got: {}", r.out);
  assert!(r.out.contains("x (1)") && r.out.contains("y (1)"), "got: {}", r.out);
}

#[test]
fn list_json_is_machine_readable() {
  // `scsh list --json` emits the profiles + their skills as JSON on stdout — a stable shape a
  // tool can parse without scraping the human listing. It's runtime-free (only git + a valid
  // .scsh.yml), so it's the programmatic way to discover profiles.
  let d = unique_dir("listjson");
  git_init(&d);
  assert_eq!(scsh(&d, &["--init-demo-project"]).code, 0);
  let r = scsh(&d, &["list", "--json"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("\"profiles\""), "got: {}", r.out);
  // The reserved `default` (add + subtract + demo-pr) and the declared `multiply`.
  assert!(
    r.out.contains(
      r#"{ "name": "default", "skills": ["add-opencode-gpt-5.4-mini-fast", "add-claude-sonnet-4-6", "subtract-opencode-gpt-5.4-mini-fast", "demo-pr-claude-sonnet", "demo-pr-codex-gpt-5.5", "demo-pr-grok-build", "demo-pr-cursor-composer-fast"] }"#
    ),
    "got: {}",
    r.out
  );
  assert!(
    r.out.contains(
      r#"{ "name": "multiply", "skills": ["multiply-opencode-gpt-5.4-mini-fast", "multiply-claude-sonnet-4-6"] }"#
    ),
    "got: {}",
    r.out
  );
  // --json is list-only (parse-time rejection, exit 2).
  let bad = scsh(&d, &["run", "--json"]);
  assert_eq!(bad.code, 2, "got: {}", bad.out);
  assert!(bad.out.contains("--json only applies to 'list'"), "got: {}", bad.out);
}

#[test]
fn check_profile_gates_on_existence_and_non_emptiness() {
  // `scsh check-profile <name>` is the runtime-free yes/no for scripts: exit 0 iff the profile
  // exists with at least one skill.
  let d = unique_dir("checkprofile");
  git_init(&d);
  assert_eq!(scsh(&d, &["--init-demo-project"]).code, 0);
  // The reserved default (add) and the declared multiply both exist with >=1 skill.
  assert_eq!(scsh(&d, &["check-profile", "default"]).code, 0);
  assert_eq!(scsh(&d, &["check-profile", "multiply"]).code, 0);
  // An unknown profile fails (exit 1) and lists the real ones.
  let ghost = scsh(&d, &["check-profile", "ghost"]);
  assert_eq!(ghost.code, 1, "got: {}", ghost.out);
  assert!(ghost.out.contains("no such profile 'ghost'"), "got: {}", ghost.out);
  assert!(ghost.out.contains("available: default, multiply"), "got: {}", ghost.out);
  // A missing name is a usage error (exit 2).
  assert_eq!(scsh(&d, &["check-profile"]).code, 2);
}

#[test]
fn check_profile_treats_an_empty_default_as_absent() {
  // When every skill is profiled, the reserved `default` exists but is empty → non-zero, while
  // the declared profile passes. `list --json` shows default with an empty skills array.
  let d = unique_dir("emptydefaultcheck");
  git_init(&d);
  std::fs::write(d.join(".scsh.yml"), include_str!("fixtures/single-profile-a.scsh.yml")).unwrap();
  let r = scsh(&d, &["check-profile", "default"]);
  assert_eq!(r.code, 1, "got: {}", r.out);
  assert!(r.out.contains("has no skills"), "got: {}", r.out);
  assert_eq!(scsh(&d, &["check-profile", "x"]).code, 0);
  let j = scsh(&d, &["list", "--json"]);
  assert_eq!(j.code, 0, "got: {}", j.out);
  assert!(j.out.contains(r#"{ "name": "default", "skills": [] }"#), "got: {}", j.out);
}

#[test]
fn harness_smoke_profile_in_tool_repo() {
  let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let r = scsh(&root, &["check-profile", "harness-smoke"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("3 skills"), "got: {}", r.out);
  let j = scsh(&root, &["list", "--json"]);
  assert_eq!(j.code, 0, "got: {}", j.out);
  assert!(j.out.contains("harness-smoke-claude-opus-4-8"), "got: {}", j.out);
  assert!(j.out.contains("harness-smoke-codex-gpt-5.5"), "got: {}", j.out);
  assert!(j.out.contains("harness-smoke-cursor-composer-fast"), "got: {}", j.out);
}

/// Full claude + codex + cursor container smoke (builds images, calls models). Requires a
/// **clean** git tree, a running container runtime, and host claude/codex/cursor auth. Run
/// manually:
///
/// ```sh
/// ./scripts/harness-smoke.sh
/// ```
///
/// or: `cargo test harness_smoke_end_to_end -- --ignored --nocapture`
#[test]
#[ignore]
fn harness_smoke_end_to_end() {
  let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  if !git_clean(&root) {
    panic!("harness-smoke e2e needs a clean git tree in {}", root.display());
  }
  let script = root.join("scripts/harness-smoke.sh");
  let output = Command::new(&script).current_dir(&root).env("SCSH", bin()).output().expect("run harness-smoke.sh");
  let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
  out.push_str(&String::from_utf8_lossy(&output.stderr));
  assert_eq!(output.status.code(), Some(0), "harness-smoke.sh failed:\n{out}");
  assert!(out.contains("harness-smoke: PASS"), "got:\n{out}");
}

#[test]
fn init_demo_refuses_to_overwrite() {
  let d = unique_dir("nooverwrite");
  git_init(&d);
  assert_eq!(scsh(&d, &["--init-demo-project"]).code, 0);
  let second = scsh(&d, &["--init-demo-project"]);
  assert_eq!(second.code, 1);
  assert!(second.out.contains("already exists"), "got: {}", second.out);
}

#[test]
fn invalid_schema_is_reported() {
  let d = unique_dir("badschema");
  git_init(&d);
  // version/project are no longer schema keys, and there are no skills — all reported.
  std::fs::write(d.join(".scsh.yml"), "version: 9\nproject: x\n").unwrap();
  let r = scsh(&d, &["list"]);
  assert_eq!(r.code, 1);
  assert!(r.out.contains("does not match the schema"), "got: {}", r.out);
  assert!(r.out.contains("unknown top-level key 'version'"), "got: {}", r.out);
  assert!(r.out.contains("missing required key 'skills'"), "got: {}", r.out);
}

#[test]
fn real_run_requires_tmp_gitignored() {
  // A clean repo+config but no /tmp ignore: the default run must stop at the /tmp
  // guard, before any container build (so this needs no network). The config is
  // committed (so the tree is clean and the run reaches the /tmp guard) but
  // .gitignore deliberately omits /tmp.
  let d = unique_dir("notmp");
  git_init(&d);
  std::fs::write(d.join(".scsh.yml"), include_str!("fixtures/minimal-opencode.scsh.yml")).unwrap();
  git(&d, &["add", "-A"]);
  git(&d, &["commit", "-qm", "config"]);
  let r = scsh(&d, &["run"]);
  assert_eq!(r.code, 1, "got: {}", r.out);
  assert!(r.out.contains("/tmp is not gitignored"), "got: {}", r.out);
  assert!(r.out.contains(".gitignore"), "got: {}", r.out);
}

#[test]
fn init_demo_commits_a_ready_to_run_project() {
  // --init-demo-project initializes the project FULLY: it scaffolds the config and
  // skills, gitignores /tmp, and commits the lot, leaving a clean working tree so
  // the very next `scsh` can run (a real run clones committed state).
  let d = unique_dir("ready");
  git_init(&d);
  let init = scsh(&d, &["--init-demo-project"]);
  assert_eq!(init.code, 0, "got: {}", init.out);
  assert!(init.out.contains("committed the scaffold"), "init should commit the scaffold; got: {}", init.out);

  // The working tree is clean...
  assert!(git_clean(&d), "init-demo should leave a clean tree; status = {:?}", {
    let o = Command::new("git").args(["status", "--porcelain"]).current_dir(&d).output().unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
  });
  // ...there is a commit, and the config is tracked.
  assert!(
    Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&d).status().unwrap().success(),
    "init-demo should create a commit"
  );
  let tracked = Command::new("git").args(["ls-files", ".scsh.yml"]).current_dir(&d).output().unwrap();
  assert!(!tracked.stdout.is_empty(), ".scsh.yml should be tracked after init-demo");

  // git itself agrees the repo-root tmp/ is ignored.
  let ignored = Command::new("git")
    .args(["check-ignore", "-q", "tmp"])
    .current_dir(&d)
    .status()
    .expect("git check-ignore")
    .success();
  assert!(ignored, "init-demo should make /tmp gitignored");
}

#[test]
fn real_run_refuses_a_dirty_working_tree() {
  // init-demo leaves a clean committed project; dirtying ANY tracked file must then
  // stop a real run (the change wouldn't be in the committed-state clone). The whole
  // tree is checked, not just the config/skills. Network-free — refused up front.
  let d = unique_dir("dirty");
  git_init(&d);
  assert_eq!(scsh(&d, &["--init-demo-project"]).code, 0);
  assert!(git_clean(&d), "init-demo should leave a clean tree");
  // Modify a committed tracked file (init-demo committed .gitignore).
  let gi = d.join(".gitignore");
  let mut s = std::fs::read_to_string(&gi).unwrap();
  s.push_str("\n# dirty\n");
  std::fs::write(&gi, s).unwrap();
  let r = scsh(&d, &["run"]);
  assert_eq!(r.code, 1, "got: {}", r.out);
  assert!(r.out.contains("clone of committed state"), "got: {}", r.out);
  assert!(r.out.contains(".gitignore"), "got: {}", r.out);
}

#[test]
fn init_demo_does_not_duplicate_existing_tmp_ignore() {
  // If /tmp is already ignored, --init-demo-project must not append a second rule.
  let d = unique_dir("initignore2");
  git_init(&d);
  std::fs::write(d.join(".gitignore"), "/tmp\n").unwrap();
  assert_eq!(scsh(&d, &["--init-demo-project"]).code, 0);
  let gi = std::fs::read_to_string(d.join(".gitignore")).unwrap();
  assert_eq!(gi.matches("/tmp").count(), 1, "should not duplicate the /tmp rule; got: {gi:?}");
}

#[test]
fn init_demo_scaffolds_example_skills() {
  // --init-demo-project drops the add/subtract/multiply example skills under .skills/ and
  // tells the user how to run them.
  let d = unique_dir("initskills");
  git_init(&d);
  let init = scsh(&d, &["--init-demo-project"]);
  assert_eq!(init.code, 0, "got: {}", init.out);

  for name in ["add", "subtract", "multiply", "demo-pr"] {
    let p = d.join(".skills").join(name).join("SKILL.md");
    let body = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    assert!(body.contains(&format!("name: {name}")), "{} should be the {name} skill", p.display());
    // Each skill ships its own worker script, scaffolded executable.
    let script_name = if name == "demo-pr" { "demo_pr.py".to_string() } else { format!("{name}.py") };
    let script = d.join(".skills").join(name).join("scripts").join(script_name);
    assert!(script.is_file(), "{} should be scaffolded", script.display());
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mode = std::fs::metadata(&script).unwrap().permissions().mode();
      assert!(mode & 0o111 != 0, "{} should be executable (mode {:o})", script.display(), mode);
    }
  }
  // It explains how to run them, including env passing (success + error) and the profile.
  assert!(init.out.contains("scaffolded 8 example-skill files"), "got: {}", init.out);
  assert!(init.out.contains("A=10 B=20 scsh run"), "init should show env-forwarding examples; got: {}", init.out);
  assert!(init.out.contains("--profile multiply"), "init should show the profile usage; got: {}", init.out);
  assert!(init.out.contains("REFUSED"), "init should show an error example too; got: {}", init.out);

  // Per the repo convention, each harness's discovery dir is symlinked at .skills/,
  // so the harness finds the skills. Check a couple resolve to the scaffolded ones.
  for host in [".opencode/skills", ".claude/skills"] {
    let link = d.join(host);
    assert!(link.symlink_metadata().expect("symlink meta").file_type().is_symlink(), "{host} should be a symlink");
    assert!(link.join("add").join("SKILL.md").is_file(), "{host} should resolve to the skills");
  }
  assert!(init.out.contains("harness skill dir") && init.out.contains("→ .skills"), "got: {}", init.out);
}

#[test]
fn init_demo_does_not_overwrite_existing_skill() {
  // A skill the user already has must be kept verbatim, never clobbered.
  let d = unique_dir("initskills2");
  git_init(&d);
  let add = d.join(".skills/add/SKILL.md");
  std::fs::create_dir_all(add.parent().unwrap()).unwrap();
  std::fs::write(&add, "name: add\nMINE — do not touch\n").unwrap();

  let init = scsh(&d, &["--init-demo-project"]);
  assert_eq!(init.code, 0, "got: {}", init.out);
  assert_eq!(
    std::fs::read_to_string(&add).unwrap(),
    "name: add\nMINE — do not touch\n",
    "existing skill was overwritten"
  );
  assert!(init.out.contains("kept existing"), "init should report the kept skill; got: {}", init.out);
  // The other skill is still scaffolded.
  assert!(d.join(".skills/multiply/SKILL.md").is_file());
}

#[test]
fn ui_demo_frames_render_the_collapsible_timestamped_board() {
  // The hidden `__ui-demo --frames` dumps deterministic frames of the interactive live board,
  // so its layout is testable in CI without a TTY: the ▶/▼ triangles, the per-line `+<elapsed>`
  // timestamps, expand vs. collapse, the ✓/✗ headers, and a scrolled window.
  let d = unique_dir("uidemo"); // needs no git repo — the demo runs nothing real
  let r = scsh(&d, &["__ui-demo", "--frames"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  // Collapsed: a closed triangle heads each proc row, with a [N] keyboard-toggle hint.
  assert!(r.out.contains("▶ "), "a collapsed ▶ triangle should be present; got: {}", r.out);
  assert!(r.out.contains("[0]") && r.out.contains("[1]"), "rows carry their shortcut hint; got: {}", r.out);
  // Expanded: an open triangle, plus output lines each stamped relative to the proc's start.
  assert!(r.out.contains("▼ "), "an expanded ▼ triangle should appear; got: {}", r.out);
  assert!(r.out.contains("+0.3s") && r.out.contains("STEP 1/3"), "timestamped build output; got: {}", r.out);
  assert!(r.out.contains("+1.5s") && r.out.contains("2 + 3 = 5"), "timestamped skill output; got: {}", r.out);
  // Finished procs show ✓/✗ with their detail.
  assert!(r.out.contains("✓ using podman"), "a ✓ header; got: {}", r.out);
  assert!(r.out.contains("✗ opencode: multiply") && r.out.contains("X is required"), "a ✗ header; got: {}", r.out);
  // Scrolling: expand opens at the proc header; the first window shows the head of the output.
  assert!(
    r.out.contains("scanning file 1") && r.out.contains("scroll down for the rest"),
    "scroll window; got: {}",
    r.out
  );
}

/// Mirror of `runtime::claude_container_auth_ready` for the "claude unavailable" guard: the
/// binary is a `[[bin]]` (no lib target to import), so this reproduces its checks — env token,
/// `~/.claude/.credentials.json`, and (macOS) the login-keychain credentials scsh forwards.
/// Keep in sync with production, or a host with keychain-only creds mis-guards the test.
fn claude_container_auth_ready() -> bool {
  std::env::var("CLAUDE_CODE_OAUTH_TOKEN").map(|s| !s.is_empty()).unwrap_or(false)
    || std::env::var_os("HOME")
      .map(PathBuf::from)
      .is_some_and(|home| home.join(".claude").join(".credentials.json").is_file())
    || claude_keychain_creds_present()
}

/// True when the macOS login keychain holds Claude Code credentials (the `claudeAiOauth`
/// blob scsh forwards). Always false off macOS.
fn claude_keychain_creds_present() -> bool {
  if !cfg!(target_os = "macos") {
    return false;
  }
  Command::new("security")
    .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
    .output()
    .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("claudeAiOauth"))
    .unwrap_or(false)
}

fn claude_integration_ready() -> bool {
  claude_container_auth_ready()
}

fn opencode_auth_ready() -> bool {
  std::env::var_os("HOME").map(PathBuf::from).is_some_and(|home| {
    let xdg = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    xdg.join("opencode").join("auth.json").is_file()
  })
}

#[test]
fn run_skips_claude_skills_when_claude_unavailable() {
  if claude_container_auth_ready() {
    eprintln!("N/A: run_skips_claude_skills_when_claude_unavailable — claude credentials configured on this host");
    return;
  }
  if !opencode_auth_ready() {
    eprintln!("N/A: run_skips_claude_skills_when_claude_unavailable — need opencode auth on this host");
    return;
  }
  let d = unique_dir("noclaude");
  git_init(&d);
  std::fs::create_dir_all(d.join(".skills/add/scripts")).unwrap();
  std::fs::write(d.join(".skills/add/SKILL.md"), include_str!("../.skills/add/SKILL.md")).unwrap();
  std::fs::write(d.join(".skills/add/scripts/add.py"), include_str!("../.skills/add/scripts/add.py")).unwrap();
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(d.join(".skills/add/scripts/add.py")).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(d.join(".skills/add/scripts/add.py"), perms).unwrap();
  }
  std::fs::write(d.join(".gitignore"), "/tmp\n").unwrap();
  std::fs::write(d.join(".scsh.yml"), include_str!("fixtures/two-route-demo.scsh.yml")).unwrap();
  git(&d, &["add", "-A"]);
  git(&d, &["commit", "-m", "two-route demo"]);
  let r = scsh(&d, &["run"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("skipping 'add-claude-sonnet-4-6'"), "got: {}", r.out);
  assert!(r.out.contains("add-opencode-gpt-5.4-mini-fast") && r.out.contains("2 + 3 = 5"), "got: {}", r.out);
}

#[test]
fn run_fails_when_every_selected_harness_unavailable() {
  if claude_container_auth_ready() {
    eprintln!("N/A: run_fails_when_every_selected_harness_unavailable — claude credentials configured on this host");
    return;
  }
  let d = unique_dir("noharness");
  git_init(&d);
  std::fs::write(d.join(".scsh.yml"), include_str!("fixtures/claude-only.scsh.yml")).unwrap();
  std::fs::create_dir_all(d.join(".skills/add")).unwrap();
  std::fs::write(d.join(".skills/add/SKILL.md"), "x").unwrap();
  std::fs::write(d.join(".gitignore"), "/tmp\n").unwrap();
  git(&d, &["add", "-A"]);
  git(&d, &["commit", "-m", "claude only"]);
  let r = scsh(&d, &["run"]);
  assert_ne!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("no skills to run") || r.out.contains("every selected skill was skipped"), "got: {}", r.out);
}

#[test]
fn claude_add_skill_runs_when_configured() {
  if !claude_integration_ready() {
    eprintln!(
      "N/A: claude_add_skill_runs_when_configured — need CLAUDE_CODE_OAUTH_TOKEN or ~/.claude/.credentials.json for container runs"
    );
    return;
  }
  let d = unique_dir("clauderun");
  git_init(&d);
  std::fs::create_dir_all(d.join(".skills/add/scripts")).unwrap();
  std::fs::write(d.join(".skills/add/SKILL.md"), include_str!("../.skills/add/SKILL.md")).unwrap();
  std::fs::write(d.join(".skills/add/scripts/add.py"), include_str!("../.skills/add/scripts/add.py")).unwrap();
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(d.join(".skills/add/scripts/add.py")).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(d.join(".skills/add/scripts/add.py"), perms).unwrap();
  }
  std::fs::write(d.join(".gitignore"), "/tmp\n").unwrap();
  std::fs::write(d.join(".scsh.yml"), include_str!("fixtures/claude-add.scsh.yml")).unwrap();
  git(&d, &["add", "-A"]);
  git(&d, &["commit", "-m", "claude-only demo"]);
  let r = scsh(&d, &["run"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(d.join("tmp/add_claude_sonnet_4_6_result.json").is_file(), "claude skill should write result");
  let body = std::fs::read_to_string(d.join("tmp/add_claude_sonnet_4_6_result.json")).unwrap();
  assert!(body.contains("2 + 3 = 5") || body.contains("result"), "got: {body}");
}

fn unused_local_port() -> u16 {
  std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Run `scsh daemon <sub>` with the test's port AND a temp SCSH_HOME, so the spawned daemon's
/// redb store lands under the test's own dir and never touches the real `~/.scsh`.
fn daemon_cmd(dir: &Path, home: &Path, port: u16, sub: &str) -> std::process::Output {
  Command::new(bin())
    .args(["daemon", sub])
    .env("SCSH_DAEMON_PORT", port.to_string())
    .env("SCSH_HOME", home)
    .current_dir(dir)
    .output()
    .unwrap_or_else(|e| panic!("scsh daemon {sub}: {e}"))
}

#[test]
fn daemon_start_status_stop() {
  let d = unique_dir("daemon");
  let home = d.join(".scsh");
  let port = unused_local_port();
  let _guard = DaemonTestGuard { dir: d.clone(), home: home.clone(), port };
  let start = daemon_cmd(&d, &home, port, "start");
  assert!(start.status.success(), "daemon start: {}", String::from_utf8_lossy(&start.stderr));
  let status = {
    let mut last = None;
    for _ in 0..40 {
      let status = daemon_cmd(&d, &home, port, "status");
      let ok = status.status.success();
      last = Some(status);
      if ok {
        break;
      }
      std::thread::sleep(std::time::Duration::from_millis(50));
    }
    last.expect("daemon status")
  };
  assert!(status.status.success(), "daemon status: {}", String::from_utf8_lossy(&status.stderr));
  let html = daemon_http_get("/", port).expect("GET /");
  assert!(html.contains("data-tab=\"jobs\""), "got: {}", html);
  let stop = daemon_cmd(&d, &home, port, "stop");
  assert!(stop.status.success(), "daemon stop: {}", String::from_utf8_lossy(&stop.stderr));
}

#[test]
fn daemon_restart() {
  let d = unique_dir("daemon-restart");
  let home = d.join(".scsh");
  let port = unused_local_port();
  let _guard = DaemonTestGuard { dir: d.clone(), home: home.clone(), port };
  let start = daemon_cmd(&d, &home, port, "start");
  assert!(start.status.success(), "daemon start: {}", String::from_utf8_lossy(&start.stderr));
  let restart = daemon_cmd(&d, &home, port, "restart");
  assert!(restart.status.success(), "daemon restart: {}", String::from_utf8_lossy(&restart.stderr));
  let html = daemon_http_get("/", port).expect("GET / after restart");
  assert!(html.contains("data-tab=\"jobs\""), "got: {}", html);
  let stop = daemon_cmd(&d, &home, port, "stop");
  assert!(stop.status.success(), "daemon stop: {}", String::from_utf8_lossy(&stop.stderr));
}

struct DaemonTestGuard {
  dir: std::path::PathBuf,
  home: std::path::PathBuf,
  port: u16,
}

impl Drop for DaemonTestGuard {
  fn drop(&mut self) {
    let _ = daemon_cmd(&self.dir, &self.home, self.port, "stop");
    let daemon_dir = std::env::temp_dir().join("scsh-daemon");
    let _ = std::fs::remove_file(daemon_dir.join(format!("daemon-{}.pid", self.port)));
    let _ = std::fs::remove_file(daemon_dir.join(format!("daemon-{}.mode", self.port)));
    // The redb store lives under `home` (inside `dir`); removing `dir` clears it.
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

fn daemon_http_get(path: &str, port: u16) -> Option<String> {
  use std::io::{Read, Write};
  use std::net::TcpStream;
  use std::time::Duration;
  let mut stream =
    TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().ok()?, Duration::from_millis(500)).ok()?;
  stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
  let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
  stream.write_all(req.as_bytes()).ok()?;
  let mut resp = String::new();
  stream.read_to_string(&mut resp).ok()?;
  Some(resp)
}

// --- export-cast -------------------------------------------------------------
// `scsh export-cast` renders a recording (+ its `.chapters.json` sidecar) into ONE
// self-contained offline HTML player page. These tests need no container and no network:
// the fixture cast is synthesized right here.

/// A tiny synthetic asciicast v3 recording (interval timestamps).
const EXPORT_CAST: &str =
  "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.5,\"o\",\"hello from scsh\\r\\n\"]\n[1.5,\"o\",\"all done\\r\\n\"]\n";

/// The matching `annotate-cast`-style sidecar: a summary plus two chapters.
const EXPORT_SIDECAR: &str = "{\n  \"summary\": \"A tiny demo run.\",\n  \"chapters\": \
[{ \"t\": 0, \"title\": \"Greeting\" }, { \"t\": 1.5, \"title\": \"Wrap-up\" }]\n}\n";

/// Like [`scsh`], but keeps stdout and stderr apart — `export-cast -o -` puts the page
/// itself on stdout, so the streams must be told apart to assert on them.
fn scsh_split(dir: &Path, args: &[&str]) -> (i32, String, String) {
  let output = Command::new(bin()).args(args).current_dir(dir).output().expect("run scsh");
  (
    output.status.code().unwrap_or(-1),
    String::from_utf8_lossy(&output.stdout).into_owned(),
    String::from_utf8_lossy(&output.stderr).into_owned(),
  )
}

#[test]
fn export_cast_renders_a_self_contained_page_with_sidecar_chapters() {
  let d = unique_dir("exportcast");
  std::fs::write(d.join("rec.cast"), EXPORT_CAST).unwrap();
  std::fs::write(d.join("rec.chapters.json"), EXPORT_SIDECAR).unwrap();
  let r = scsh(&d, &["export-cast", "rec.cast"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  let html = std::fs::read_to_string(d.join("rec.html")).expect("rec.html next to the cast");
  // The player boots and the recording's own text is embedded (as escaped script data).
  assert!(html.contains("BeeCastPlayer.create"), "player boot missing");
  assert!(html.contains("hello from scsh"), "cast data not embedded");
  // Self-contained: no external scripts, stylesheets, or http(s) resource references
  // (the same inline-safety bar beecast holds its own pages to).
  assert!(!html.contains("<script src"), "no external scripts");
  assert!(!html.contains("<link rel=\"stylesheet\""), "no external stylesheets");
  assert!(!html.contains("src=\"http") && !html.contains("href=\"http"), "no http(s) resource references");
  // The sidecar's summary + chapters render into the page; the cast's stem is the title.
  assert!(html.contains("<title>rec</title>"), "stem should be the title");
  assert!(html.contains("A tiny demo run."), "summary missing");
  assert!(html.contains("Greeting") && html.contains("Wrap-up"), "chapters missing");
  // The embedded player is the first-party beecast-player (born here as
  // scsh-cast-player): no third-party code — and no second license — may ride in any
  // exported page. A beecast-page upgrade that reintroduces either has to fail here.
  assert!(html.contains("BeeCastPlayer"), "the first-party player must be embedded");
  for banned in ["@license", "Apache", "asciinema-player", "AsciinemaPlayer"] {
    assert!(!html.contains(banned), "third-party marker '{banned}' in an exported page");
  }
  // stdout was piped → the machine document, one entry with the per-cast facts.
  assert!(r.out.contains("\"exported\""), "got: {}", r.out);
  assert!(r.out.contains("\"chapters\": 2") && r.out.contains("rec.html"), "got: {}", r.out);
}

#[test]
fn export_cast_without_a_sidecar_uses_the_stem_title() {
  let d = unique_dir("exportnosidecar");
  std::fs::write(d.join("bare.cast"), EXPORT_CAST).unwrap();
  let r = scsh(&d, &["export-cast", "bare.cast"]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  let html = std::fs::read_to_string(d.join("bare.html")).expect("bare.html next to the cast");
  assert!(html.contains("<title>bare</title>"), "stem title fallback");
  assert!(r.out.contains("\"chapters\": 0"), "got: {}", r.out);
}

#[test]
fn export_cast_streams_the_page_to_stdout_with_dash() {
  let d = unique_dir("exportstdout");
  std::fs::write(d.join("rec.cast"), EXPORT_CAST).unwrap();
  std::fs::write(d.join("rec.chapters.json"), EXPORT_SIDECAR).unwrap();
  let (code, stdout, _stderr) = scsh_split(&d, &["export-cast", "rec.cast", "-o", "-"]);
  assert_eq!(code, 0, "got: {stdout}");
  // stdout IS the page — nothing else: no report document mixed into the HTML.
  assert!(stdout.starts_with("<!DOCTYPE html>"), "got: {}", &stdout[..stdout.len().min(80)]);
  assert!(stdout.contains("BeeCastPlayer.create") && stdout.contains("Greeting"), "page content on stdout");
  assert!(!stdout.contains("\"exported\""), "no JSON report in the streamed page");
  assert!(!d.join("rec.html").exists(), "-o - must not also write a file");
}

#[test]
fn export_cast_rejects_a_non_cast_but_exports_the_rest() {
  let d = unique_dir("exportbadcast");
  std::fs::write(d.join("junk.cast"), "definitely not an asciicast\n").unwrap();
  std::fs::write(d.join("rec.cast"), EXPORT_CAST).unwrap();
  let r = scsh(&d, &["export-cast", "junk.cast", "rec.cast"]);
  // The bad cast is an actionable failure (✗ what's wrong / → how to fix, exit 1)…
  assert_eq!(r.code, 1, "got: {}", r.out);
  assert!(r.out.contains("\u{2717}") && r.out.contains("not an asciicast"), "got: {}", r.out);
  assert!(r.out.contains("\u{2192}") && r.out.contains("asciinema recording"), "got: {}", r.out);
  // …but the good cast in the same invocation still exports.
  assert!(d.join("rec.html").exists(), "the good cast should still export");
  assert!(!d.join("junk.html").exists(), "the bad cast must not produce a page");
}

#[test]
fn export_cast_o_with_two_casts_is_a_usage_error() {
  let d = unique_dir("exporttwocasts");
  std::fs::write(d.join("a.cast"), EXPORT_CAST).unwrap();
  std::fs::write(d.join("b.cast"), EXPORT_CAST).unwrap();
  let r = scsh(&d, &["export-cast", "a.cast", "b.cast", "-o", "out.html"]);
  assert_eq!(r.code, 2, "got: {}", r.out);
  assert!(r.out.contains("-o applies to exactly one cast"), "got: {}", r.out);
  assert!(!d.join("out.html").exists() && !d.join("a.html").exists(), "a usage error must export nothing");
}

#[test]
fn export_cast_malformed_sidecar_warns_but_exports() {
  let d = unique_dir("exportbadsidecar");
  std::fs::write(d.join("rec.cast"), EXPORT_CAST).unwrap();
  std::fs::write(d.join("rec.chapters.json"), "{ not json at all").unwrap();
  let r = scsh(&d, &["export-cast", "rec.cast"]);
  // A malformed sidecar is a warning on stderr AND in the machine document — never a failure.
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("malformed sidecar"), "got: {}", r.out);
  assert!(r.out.contains("\"warning\""), "the JSON entry should carry the warning; got: {}", r.out);
  let html = std::fs::read_to_string(d.join("rec.html")).expect("export proceeds without the sidecar");
  assert!(html.contains("<title>rec</title>") && !html.contains("\"chapters\":["), "no chapters from junk");
}

// ---- run --def <name> (harness definitions) ------------------------------------------

#[test]
fn run_def_needs_a_name() {
  let d = unique_dir("defnoname"); // parse error happens before any preflight
  let r = scsh(&d, &["run", "--def"]);
  assert_eq!(r.code, 2, "got: {}", r.out);
  assert!(r.out.contains("--def needs a harness-definition name"), "got: {}", r.out);
}

#[test]
fn run_def_rejects_a_profile() {
  let d = unique_dir("defprofile");
  let r = scsh(&d, &["run", "--def", "add", "--profile", "x"]);
  assert_eq!(r.code, 2, "got: {}", r.out);
  assert!(r.out.contains("--def selects a harness definition, not a profile"), "got: {}", r.out);
}

#[test]
fn def_only_applies_to_run() {
  let d = unique_dir("deflist");
  let r = scsh(&d, &["list", "--def", "add"]);
  assert_eq!(r.code, 2, "got: {}", r.out);
  assert!(r.out.contains("--def only applies to 'run'"), "got: {}", r.out);
}

#[test]
fn run_def_unknown_definition_lists_available() {
  // A clean repo with /tmp gitignored gets past the repo-hygiene preflight to definition
  // discovery, where an unknown name is reported with the available (built-in) definitions.
  let d = unique_dir("defunknown");
  git_init(&d);
  std::fs::write(d.join(".gitignore"), "/tmp\n").unwrap();
  git(&d, &["add", "-A"]);
  git(&d, &["commit", "-qm", "init"]);
  assert!(git_clean(&d));
  let r = scsh(&d, &["run", "--def", "nonesuch"]);
  assert_eq!(r.code, 1, "got: {}", r.out);
  assert!(r.out.contains("no harness definition named 'nonesuch'"), "got: {}", r.out);
  assert!(
    r.out.contains("add") && r.out.contains("doctor") && r.out.contains("research"),
    "lists built-ins; got: {}",
    r.out
  );
}

// ---- --override-dot-scsh-yml ---------------------------------------------------------

#[test]
fn override_yml_needs_a_path() {
  let d = unique_dir("ovrynopath");
  let r = scsh(&d, &["run", "--override-dot-scsh-yml"]);
  assert_eq!(r.code, 2, "got: {}", r.out);
  assert!(r.out.contains("--override-dot-scsh-yml needs a path"), "got: {}", r.out);
}

#[test]
fn override_yml_rejects_def() {
  let d = unique_dir("ovrydef");
  let r = scsh(&d, &["run", "--def", "add", "--override-dot-scsh-yml", "/tmp/x.scsh.yml"]);
  assert_eq!(r.code, 2, "got: {}", r.out);
  assert!(r.out.contains("mutually exclusive"), "got: {}", r.out);
}

#[test]
fn override_yml_only_on_run_list_check() {
  let d = unique_dir("ovrybad");
  let r = scsh(&d, &["version", "--override-dot-scsh-yml", "/tmp/x.scsh.yml"]);
  assert_eq!(r.code, 2, "got: {}", r.out);
  assert!(r.out.contains("only applies to 'run', 'list', and 'check-profile'"), "got: {}", r.out);
}

#[test]
fn override_yml_check_profile_uses_external_config() {
  // A clean repo with NO .scsh.yml of its own can still check a profile via an external
  // override bundle (the global /code-fantastic-review path).
  let d = unique_dir("ovrycheck");
  git_init(&d);
  std::fs::write(d.join(".gitignore"), "/tmp\n").unwrap();
  git(&d, &["add", "-A"]);
  git(&d, &["commit", "-qm", "init"]);

  let bundle = unique_dir("ovrybundle");
  std::fs::create_dir_all(bundle.join(".skills/conventions-reviewer")).unwrap();
  std::fs::write(
    bundle.join(".scsh.yml"),
    r#"skills:
  conventions-reviewer:
    profile: code-review
    timeout: 60
    result: tmp/code-review-conventions-reviewer-{name}.json
    invocations:
      codex-gpt-5.5:
        harness: codex
        model: gpt-5.5
"#,
  )
  .unwrap();
  std::fs::write(bundle.join(".skills/conventions-reviewer/SKILL.md"), "# conventions-reviewer\n").unwrap();

  let yml = bundle.join(".scsh.yml");
  let r = scsh(&d, &["check-profile", "code-review", "--override-dot-scsh-yml", yml.to_str().unwrap()]);
  assert_eq!(r.code, 0, "got: {}", r.out);
  assert!(r.out.contains("profile 'code-review'"), "got: {}", r.out);

  // Without the override, the same repo has no .scsh.yml → fail.
  let bare = scsh(&d, &["check-profile", "code-review"]);
  assert_ne!(bare.code, 0, "got: {}", bare.out);
  assert!(bare.out.contains(".scsh.yml not found"), "got: {}", bare.out);
}
