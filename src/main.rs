//! scsh — Scoped Skills Helper.
//!
//! Preflight a git repository (git → repo → `.scsh.yml` present → schema-valid →
//! a container runtime), then build one in-memory image and run the project's
//! scoped skills — all of them, in parallel, each in its own ephemeral container
//! under its configured harness.

mod config;
mod json;
mod runtime;
mod ui;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use config::Skill;
use runtime::Runtime;

fn main() {
  let args: Vec<String> = std::env::args().skip(1).collect();
  std::process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
  let cli = match parse_cli(args) {
    Ok(c) => c,
    Err(e) => {
      eprintln!("scsh: {e}");
      eprintln!("try 'scsh --help'");
      return 2;
    }
  };
  let profile = cli.profile.as_deref();
  match cli.mode {
    Mode::Help(topic) => {
      print_help(topic);
      0
    }
    Mode::Version => {
      println!("scsh {}", version_id());
      0
    }
    Mode::Run => preflight_then(Action::Run, profile),
    // Hidden: a self-contained demo of the live board (no container/model needed), used by the
    // feature's demo + PTY test. `--frames` dumps deterministic plain frames; otherwise it runs
    // the real interactive board over a few scripted subprocesses.
    Mode::UiDemo { frames } => ui::demo::run(frames),
  }
}

#[derive(Clone, Copy)]
enum Mode {
  Help(HelpTopic),
  Version,
  Run,
  UiDemo { frames: bool },
}

/// Which help page to print. The default (`scsh help` / a bare `scsh`) is a compact
/// overview of the commands; the deep-dive topics keep their detail OUT of the default
/// output (`scsh help .scsh.yml`, `scsh help internals`, `scsh help cache`).
#[derive(Clone, Copy)]
enum HelpTopic {
  Overview,
  Config,
  Internals,
  Cache,
}

/// The version identifier: the crate version, plus the git short hash (and a `-dirty`
/// marker) captured at build time by `build.rs`. Just the crate version when built
/// outside a git checkout. E.g. `0.1 (a1b2c3d-dirty)`.
///
/// Cargo's manifest requires a full semver (`X.Y.Z`), so `Cargo.toml` says `0.1.0`;
/// we display it as `0.1` (a zero patch is dropped — scsh is nowhere near 1.0).
fn version_id() -> String {
  let v = env!("CARGO_PKG_VERSION");
  let v = v.strip_suffix(".0").unwrap_or(v);
  let git = env!("SCSH_GIT_DESCRIBE");
  if git.is_empty() {
    v.to_string()
  } else {
    format!("{v} ({git})")
  }
}

enum Action {
  Run,
}

/// A parsed command line: one command, plus the profiles that select which skills run (only
/// valid for `run` — given as bare positional names, `--profile`, or both).
struct Cli {
  mode: Mode,
  profile: Option<String>,
}

/// Parse cargo-style subcommands. The default (no command) is `help`, so a bare
/// `scsh` is safe and self-explanatory; `run` is the explicit "do it" command.
/// The old `--help` / `--version` flags keep working as aliases.
fn parse_cli(args: &[String]) -> Result<Cli, String> {
  let mut mode: Option<Mode> = None;
  let mut profiles: Vec<String> = Vec::new();
  let mut frames = false;
  let mut i = 0;
  while i < args.len() {
    let m = match args[i].as_str() {
      "help" | "-h" | "--help" => {
        // An optional next token selects a deep-dive topic; otherwise the overview.
        let topic = match args.get(i + 1).map(|s| s.as_str()) {
          Some(".scsh.yml") | Some("scsh.yml") | Some(".scsh.yaml") | Some("scsh.yaml") | Some("config")
          | Some("yaml") | Some("yml") | Some("schema") => {
            i += 1;
            HelpTopic::Config
          }
          Some("internals") | Some("internal") => {
            i += 1;
            HelpTopic::Internals
          }
          Some("cache") | Some("caching") => {
            i += 1;
            HelpTopic::Cache
          }
          // A non-flag token we don't recognize is a mistyped topic — say so helpfully.
          Some(other) if !other.starts_with('-') => {
            return Err(format!("unknown help topic '{other}' (topics: .scsh.yml, internals, cache)"));
          }
          _ => HelpTopic::Overview,
        };
        Some(Mode::Help(topic))
      }
      "version" | "-V" | "--version" => Some(Mode::Version),
      "run" => Some(Mode::Run),
      // Hidden dev command: demo the live board with no container/model (see `ui::demo`).
      "__ui-demo" => Some(Mode::UiDemo { frames: false }),
      "--frames" => {
        frames = true;
        None
      }
      "--profile" | "--profiles" => {
        i += 1;
        let name = args.get(i).ok_or("--profile needs a name, e.g. --profile code-review (or default,code-review)")?;
        if name.trim().is_empty() {
          return Err("--profile name must not be empty".into());
        }
        profiles.push(name.clone());
        None
      }
      // After `run`, a bare token is a profile name: `scsh run a b` == `scsh run --profile a,b`.
      other if matches!(mode, Some(Mode::Run)) && !other.starts_with('-') => {
        profiles.push(other.to_string());
        None
      }
      other => return Err(format!("unknown command or option '{other}' (try 'scsh help')")),
    };
    if let Some(m) = m {
      if mode.is_some() {
        return Err("only one command may be given at a time".into());
      }
      mode = Some(m);
    }
    i += 1;
  }
  let mode = match mode.unwrap_or(Mode::Help(HelpTopic::Overview)) {
    Mode::UiDemo { .. } => Mode::UiDemo { frames },
    other => other,
  };
  // Positional profiles and any `--profile` values combine into one comma-joined spec.
  let profile = if profiles.is_empty() { None } else { Some(profiles.join(",")) };
  if profile.is_some() && !matches!(mode, Mode::Run) {
    return Err(
      "profiles only apply to 'run' (e.g. `scsh run code-review` or `scsh run --profile code-review`)".into(),
    );
  }
  Ok(Cli { mode, profile })
}

/// The profiles requested on the command line, as a set. No `--profile` is the reserved
/// `default` profile (the skills with no `profile:`); a spec may name several, separated by
/// `,` or `;` — e.g. `--profile default,multiply` selects both groups.
fn requested_profiles(spec: Option<&str>) -> std::collections::BTreeSet<String> {
  match spec {
    None => std::iter::once("default".to_string()).collect(),
    Some(s) => s.split([',', ';']).map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect(),
  }
}

/// The skills selected for a run: those whose profile is in the requested set, where a skill
/// with no `profile:` belongs to the reserved `default` profile. So `None` (no `--profile`)
/// selects only `default`; `--profile X` selects only X's skills; `--profile default,X` both.
fn select_skills<'a>(cfg: &'a config::Config, profile: Option<&str>) -> Vec<&'a Skill> {
  let want = requested_profiles(profile);
  cfg.skills.iter().filter(|s| want.contains(s.profile.as_deref().unwrap_or("default"))).collect()
}

/// The distinct profile names declared across a config's skills, in first-seen order.
fn declared_profiles(cfg: &config::Config) -> Vec<&str> {
  let mut out: Vec<&str> = Vec::new();
  for s in &cfg.skills {
    if let Some(p) = s.profile.as_deref() {
      if !out.contains(&p) {
        out.push(p);
      }
    }
  }
  out
}

// ---------------------------------------------------------------------------
// Preflight + actions
// ---------------------------------------------------------------------------

fn preflight_then(action: Action, profile: Option<&str>) -> i32 {
  // The preflight checks run quietly on success and collapse into one compact
  // summary line (see CONTRIBUTING "Output style"); only failures speak up, each
  // with an actionable ✗/→. A real run is ordered repo-hygiene-first:
  // git → repo → clean → /tmp → config present → config valid → runtime → engine.
  let is_run = matches!(action, Action::Run);

  // 1. git installed.
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return 1;
  }

  // 2. inside a git repository.
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return 1;
    }
  };

  // For a real run, the repo must be runnable BEFORE we look at the config: a clean
  // working tree (the container gets a clone of COMMITTED state) and a gitignored
  // /tmp (build scratch + the collected result stay untracked).
  if is_run {
    let dirty = uncommitted_changes(&root);
    if !dirty.is_empty() {
      fail("working tree has uncommitted changes — scsh runs a clone of committed state, so they would not be in the container");
      let shown = dirty.len().min(10);
      for p in &dirty[..shown] {
        hint(&format!("uncommitted: {p}"));
      }
      if dirty.len() > shown {
        hint(&format!("\u{2026}and {} more", dirty.len() - shown));
      }
      hint(&format!(
        "commit or stash them first, then re-run:  {}",
        bold("git add -A && git commit -m \"Committing unstaged changes to run scsh.\"")
      ));
      return 1;
    }
    if !tmp_is_gitignored(&root) {
      fail("/tmp is not gitignored in this repository");
      if !root.join(".scsh.yml").is_file() {
        // Fresh repo: don't make them fix this by hand — one command sets it all up.
        hint(&format!("get a ready-to-run project in one command: {}", bold("scsh init-demo-project")));
        hint("(writes .scsh.yml, gitignores /tmp, scaffolds example skills, and commits)");
      } else {
        // Has a config already: scsh still fixes the .gitignore and commits for you.
        hint(&format!("let scsh add /tmp to .gitignore and commit it: {}", bold("scsh init-demo-project")));
      }
      return 1;
    }
  }

  // 3. .scsh.yml present at the repo root.
  let cfg_path = root.join(".scsh.yml");
  if !cfg_path.is_file() {
    fail(".scsh.yml not found — this repository isn't set up for scsh yet");
    hint(&format!("get a ready-to-run project in one command: {}", bold("scsh init-demo-project")));
    hint("(writes .scsh.yml, gitignores /tmp, scaffolds example skills, and commits)");
    return 1;
  }

  // 4. .scsh.yml matches the schema.
  let src = match std::fs::read_to_string(&cfg_path) {
    Ok(s) => s,
    Err(e) => {
      fail(&format!("could not read .scsh.yml: {e}"));
      return 1;
    }
  };
  let cfg = match config::validate(&src) {
    Ok(c) => c,
    Err(errs) => {
      let n = errs.len();
      fail(&format!(".scsh.yml does not match the schema ({n} problem{})", if n == 1 { "" } else { "s" }));
      for e in &errs {
        hint(e);
      }
      hint("fix the file to match the schema (see 'scsh --help' or the README)");
      return 1;
    }
  };

  // 5. a container runtime is available.
  let rt = match runtime::detect_runtime() {
    Some(rt) => rt,
    None => {
      let cands = runtime::runtime_candidates(cfg!(target_os = "macos")).join(", ");
      fail(&format!("no container runtime found (looked for: {cands})"));
      hint(install_runtime_hint());
      return 1;
    }
  };

  // A snap-packaged Docker can't bind-mount the system temp dir where each clone
  // lives (the container would see an empty home and the skill would crash).
  // Auto-detection already prefers another runtime; warn if it's the only/forced one.
  if rt.name == "docker" && runtime::is_snap_confined(&rt.path) {
    hint("this is snap-packaged Docker, which can't bind-mount the system temp dir;");
    hint("if skills fail to start, use Podman instead (e.g. SCSH_RUNTIME=podman)");
  }

  // 6. For a real run, the runtime's engine must actually be up.
  if is_run && !ui::engine::is_running(&rt.name) {
    fail(&format!("{} is installed but not running", ui::engine::display_name(&rt.name)));
    if let Some(cmd) = ui::engine::start_command(&rt.name, ui::Os::current()) {
      hint(&format!("start it with: {}", bold(&cmd)));
    }
    hint("then re-run 'scsh run'");
    return 1;
  }

  match action {
    Action::Run => {
      // Every requested --profile must be `default` (the no-profile skills) or a profile
      // this config declares.
      if profile.is_some() {
        let declared = declared_profiles(&cfg);
        let unknown: Vec<String> = requested_profiles(profile)
          .into_iter()
          .filter(|p| p != "default" && !declared.contains(&p.as_str()))
          .collect();
        if !unknown.is_empty() {
          fail(&format!("unknown profile{}: {}", plural(unknown.len()), unknown.join(", ")));
          let mut avail = vec!["default".to_string()];
          avail.extend(declared.iter().map(|s| s.to_string()));
          hint(&format!("available: {} (see them with: scsh list)", avail.join(", ")));
          return 1;
        }
      }
      let selected = select_skills(&cfg, profile);
      if selected.is_empty() {
        let scope = profile.unwrap_or("default");
        fail(&format!("nothing to run \u{2014} the '{scope}' profile is empty"));
        hint("see the available profiles and their skills:  scsh list");
        hint("then pick one:  scsh run --profile <name>");
        return 1;
      }
      // Every git/repo/state check passed — one compact line, then the run.
      let prof = profile.map(|p| format!(" · profile {p}")).unwrap_or_default();
      ok(&format!("git · repo {} · clean · /tmp ignored{prof}", display_path(&root)));
      build_and_run(&rt, &root, &selected)
    }
  }
}

/// Friendly name for the chosen containerization backend, for the "using …" line.
/// Apple's runtime shows as "Apple Containers"; docker/podman stay lowercase.
fn backend_name(runtime: &str) -> &str {
  match runtime {
    "docker" => "docker",
    "podman" => "podman",
    "container" => "Apple Containers",
    other => other,
  }
}

/// Abbreviate a path with `~` for `$HOME` (so a repo reads as `~/1`, not a long path).
fn display_path(p: &Path) -> String {
  if let Some(home) = std::env::var_os("HOME") {
    if let Ok(rest) = p.strip_prefix(PathBuf::from(home)) {
      return if rest.as_os_str().is_empty() { "~".to_string() } else { format!("~/{}", rest.display()) };
    }
  }
  p.display().to_string()
}

/// Repo-relative paths with uncommitted changes — staged, unstaged, or untracked
/// (gitignored paths are excluded by git, so `/tmp`, `target/`, etc. never count).
/// scsh runs each skill on a clone of committed state, so a non-empty result means
/// the working tree and that clone would differ; a real run refuses until it is
/// clean. Parsed from `git status --porcelain`, so every kind of change is caught.
fn uncommitted_changes(root: &std::path::Path) -> Vec<String> {
  let status = match git_capture(root, &["status", "--porcelain"]) {
    Some(s) => s,
    None => return Vec::new(),
  };
  let mut out: Vec<String> = Vec::new();
  for line in status.lines() {
    if line.len() < 4 {
      continue;
    }
    // Porcelain is "XY <path>"; a rename shows "old -> new" — take the new path.
    let mut path = line[3..].trim();
    if let Some(idx) = path.find(" -> ") {
      path = &path[idx + 4..];
    }
    let path = path.trim_matches('"');
    if !path.is_empty() && !out.iter().any(|p| p == path) {
      out.push(path.to_string());
    }
  }
  out
}

/// Whether the repository ignores `/tmp` (a `/tmp` line in .gitignore makes the
/// repo-root path `tmp` ignored). Checked via `git check-ignore` so every
/// gitignore source is honored.
fn tmp_is_gitignored(root: &std::path::Path) -> bool {
  Command::new("git")
    .arg("-C")
    .arg(root)
    .args(["check-ignore", "-q", "tmp"])
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

fn build_and_run(rt: &Runtime, root: &std::path::Path, skills: &[&Skill]) -> i32 {
  // Isolate children into their own process groups and catch SIGINT/SIGTERM, so a stray signal
  // can't kill a container mid-run and a kill restores the terminal + tears the children down.
  ui::signals::install();
  let tag = runtime::image_tag();
  let (uid, gid) = host_ids();
  let secs = now_secs();
  // Sweep run-clones left in /tmp by past runs (failed skills' kept clones, or clones from
  // a crash before cleanup) before starting. Only dirs older than a full day are removed, so
  // a concurrently-running scsh's fresh clone is never touched. Skipped under SCSH_KEEP_RUNS=1.
  if !keep_run_dirs() {
    let swept = sweep_stale_run_dirs(secs);
    if swept > 0 {
      hint(&format!("swept {swept} stale run dir{} from /tmp", plural(swept)));
    }
  }

  // Whether the host's opencode login will be forwarded into each run. Printed BEFORE the live
  // board takes over the screen, so it stays in the normal scrollback above the final summary.
  if opencode_auth_enabled() {
    match host_opencode_auth() {
      Some(_) => ok("opencode creds found (forwarded into each skill)"),
      None => hint("no opencode creds found (~/.local/share/opencode/auth.json); skills may fail to authenticate — try 'opencode auth login'"),
    }
  }

  // The interactive live board: the image build, then every skill, each a collapsible row you
  // click to expand — its output, every line stamped with its time relative to that proc's
  // start. Scroll with the wheel / arrows. Off a TTY it degrades to plain ▶/✓/✗ lines. It owns
  // the terminal until `finish`, so nothing else should print to the screen during the run.
  let ui = ui::screen::LiveUi::new(console::user_attended_stderr());

  // 1. Build ONE generic image (opencode + a dev toolchain + the agent user) every skill's
  //    container runs from — it bakes no skill command; each run supplies its own.
  let df = runtime::dockerfile();
  let build = ui.proc(format!("using {} · build", backend_name(&rt.name)), true);
  build.start();
  if let Err((msg, code)) = run_build(&build, &rt.name, &tag, &df, uid, gid) {
    build.finish_fail(Some(&msg));
    ui.finish();
    fail(&msg);
    return code;
  }
  build.finish_ok(None);

  // 2. Run every skill in parallel: each gets its own clone and container — its own row on the
  //    board — and must produce its declared result file.
  let tag = tag.as_str();
  let outcomes: Vec<SkillRun> = std::thread::scope(|scope| {
    let handles: Vec<_> = skills
      .iter()
      .map(|&skill| {
        let p = ui.proc(format!("{}: {}", skill.harness.as_str(), skill.name), false);
        scope.spawn(move || run_one_skill(skill, rt, tag, root, secs, p))
      })
      .collect();
    handles.into_iter().map(|h| h.join().unwrap_or_else(|_| SkillRun::failed(None, None))).collect()
  });

  // The run is over: restore the terminal and print the persistent ✓/✗ summary (attended; off a
  // TTY the per-proc lines already streamed). Everything below prints to the normal screen.
  ui.finish();

  // 3. The summary above carries each skill's ✓/✗ and detail; add run-dir/log pointers for any
  //    that failed, then the overall verdict.
  let n = outcomes.len();
  let failed = outcomes.iter().filter(|o| !o.ok).count();
  for o in outcomes.iter().filter(|o| !o.ok) {
    if let Some(dir) = &o.run_dir {
      hint(&format!("run dir kept: {dir}"));
    }
    if let Some(log) = &o.log {
      hint(&format!("output log: {log}"));
    }
  }

  // A failed skill's clone is kept for inspection (its path is printed above); successful
  // clones and any older leftovers are reclaimed by the next run's stale-clone sweep. (Per-run
  // cleanup of a successful clone arrives with commit integration, which holds onto each clone.)

  if failed == 0 {
    ok(&format!("all {n} skill{} completed successfully", plural(n)));
    0
  } else {
    fail(&format!("{failed} of {n} skill{} failed", plural(n)));
    1
  }
}

/// The outcome of running one skill end to end (clone → harness → collect). The per-skill ✓/✗
/// and its detail are shown by the live board (and its final summary); this is the structured
/// residue the orchestrator still needs afterward — the run-dir/log pointers.
struct SkillRun {
  ok: bool,
  /// The `/tmp` run dir, kept for inspection when the skill failed.
  run_dir: Option<String>,
  /// Host path to the skill's output log, when its container actually ran.
  log: Option<String>,
}

impl SkillRun {
  fn ok(log: String) -> SkillRun {
    SkillRun { ok: true, run_dir: None, log: Some(log) }
  }
  fn failed(run_dir: Option<String>, log: Option<String>) -> SkillRun {
    SkillRun { ok: false, run_dir, log }
  }
}

/// Run a single skill end to end in its own clone and container, driving `spinner`
/// through its phases and finishing it ✓/✗. Returns the structured outcome.
fn run_one_skill(skill: &Skill, rt: &Runtime, tag: &str, root: &Path, secs: u64, spinner: ui::screen::Proc) -> SkillRun {
  // Mark the row running so its clock starts and output stamps are relative to here.
  spinner.start();
  // Resolve forwarded env first: a missing required (${VAR:?…}) variable refuses
  // the skill before any work — no clone, no container.
  let env = match resolve_env(&skill.env) {
    Ok(e) => e,
    Err(message) => {
      spinner.finish_fail(Some(&message));
      return SkillRun::failed(None, None);
    }
  };

  // Own clone of the repo, so parallel skills never share a working tree.
  spinner.note("cloning…");
  let run_dir = match prepare_run_dir(secs, &skill.name) {
    Ok(d) => d,
    Err(e) => {
      spinner.finish_fail(Some(&e));
      return SkillRun::failed(None, None);
    }
  };
  let run_dir_str = run_dir.to_string_lossy().into_owned();
  if let Err(e) = clone_into(root, &run_dir, &spinner) {
    spinner.finish_fail(Some(&e));
    return SkillRun::failed(Some(run_dir_str), None);
  }

  // Ensure the result's parent dir exists in the clone so the skill can write it
  // even if the harness's tool does not `mkdir -p`.
  if let Some(parent) = Path::new(&skill.result).parent() {
    if !parent.as_os_str().is_empty() {
      let _ = std::fs::create_dir_all(run_dir.join(parent));
    }
  }

  // Run the harness command in a named container with the clone mounted at /home/agent/repo,
  // under the skill's optional wall-clock timeout.
  spinner.note(&format!("{} run…", skill.harness.as_str()));
  let name = run_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| skill.name.clone());
  // The harness tees its output to this log under the mount's gitignored tmp/ (= on the host).
  // Create its parent so `tee` can write even before the skill touches tmp/.
  let log_path = run_dir.join(runtime::RUN_LOG_REL);
  if let Some(parent) = log_path.parent() {
    let _ = std::fs::create_dir_all(parent);
  }
  let log = log_path.to_string_lossy().into_owned();
  // Forward host opencode credentials so the container is authenticated; remove the
  // copy right after the run so the secret never lingers in the system temp dir.
  let auth = if opencode_auth_enabled() { forward_opencode_auth(&run_dir) } else { None };
  let cmd = runtime::harness_command(skill.harness, skill.model.as_deref(), &skill.name);
  let run = runtime::run_command(&rt.name, tag, &run_dir_str, &name, &env, &cmd);
  let timeout = skill.timeout.map(Duration::from_secs);
  let result = spinner.run_timed(&run[0], &run[1..], timeout);
  if let Some(p) = &auth {
    let _ = std::fs::remove_file(p);
  }
  match result {
    Ok((true, _, _)) => {}
    Ok((false, true, _)) => {
      // Timed out: the client was killed; stop the container too (best effort).
      kill_container(&rt.name, &name);
      let why = format!("timed out after {}s", skill.timeout.unwrap_or(0));
      spinner.finish_fail(Some(&why));
      return SkillRun::failed(Some(run_dir_str), Some(log));
    }
    Ok((false, false, last)) => {
      let why = match last {
        Some(l) if !l.is_empty() => format!("harness exited non-zero ({l})"),
        _ => "harness exited non-zero".into(),
      };
      spinner.finish_fail(Some(&why));
      return SkillRun::failed(Some(run_dir_str), Some(log));
    }
    Err(e) => {
      let why = format!("could not run container: {e}");
      spinner.finish_fail(Some(&why));
      return SkillRun::failed(Some(run_dir_str), None);
    }
  }

  // The result file is required: missing → this skill (and the whole run) fails.
  match collect_skill_result(root, &run_dir, &skill.result, secs) {
    Ok(dest) => {
      // Show the skill's *message*, not just the file (its `result`/`message`/sole field —
      // see json::message), falling back to the result path; a multi-line message shows
      // its first line.
      let content = std::fs::read_to_string(&dest).ok();
      let message = content.as_deref().and_then(json::message);
      let headline = message.as_deref().map(first_line).unwrap_or(skill.result.as_str());
      spinner.finish_ok(Some(headline));
      SkillRun::ok(log)
    }
    Err(e) => {
      spinner.finish_fail(Some(&e));
      SkillRun::failed(Some(run_dir_str), Some(log))
    }
  }
}

/// The first line of a (possibly multi-line) message, for a one-line skill report.
fn first_line(s: &str) -> &str {
  s.lines().next().unwrap_or(s)
}

/// `"s"` unless `n == 1`.
fn plural(n: usize) -> &'static str {
  if n == 1 {
    ""
  } else {
    "s"
  }
}

/// Age (seconds) past which a leftover `/tmp/scsh-*-utc-run-*` clone is treated as stale and
/// swept at the next run's startup. A full day — comfortably longer than any skill run (skill
/// timeouts are in minutes) — so a concurrently-running scsh's fresh clone is never removed.
const STALE_RUN_DIR_SECS: u64 = 24 * 60 * 60;

/// Best-effort sweep of stale per-run clones left under `/tmp` by earlier runs — a failed
/// skill's kept clone, or a clone orphaned by a crash before cleanup. Only entries matching
/// the run-dir name (`scsh-*-utc-run-*`) AND older than [`STALE_RUN_DIR_SECS`] are removed,
/// so an in-progress concurrent run is never disturbed. Returns how many were removed.
fn sweep_stale_run_dirs(now: u64) -> usize {
  sweep_stale_run_dirs_in(Path::new("/tmp"), now, STALE_RUN_DIR_SECS)
}

/// The body of [`sweep_stale_run_dirs`], parameterized by the directory to scan and the
/// staleness threshold so it can be unit-tested. A matching entry is removed only if it is a
/// directory whose mtime is at least `max_age` seconds before `now`.
fn sweep_stale_run_dirs_in(dir: &Path, now: u64, max_age: u64) -> usize {
  let mut removed = 0;
  let Ok(entries) = std::fs::read_dir(dir) else {
    return 0;
  };
  for entry in entries.flatten() {
    let name = entry.file_name();
    let name = name.to_string_lossy();
    if !(name.starts_with("scsh-") && name.contains("-utc-run-")) {
      continue;
    }
    let path = entry.path();
    if !path.is_dir() {
      continue;
    }
    let stale = std::fs::metadata(&path)
      .and_then(|m| m.modified())
      .ok()
      .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
      .map(|d| now.saturating_sub(d.as_secs()) >= max_age)
      .unwrap_or(false);
    if stale && std::fs::remove_dir_all(&path).is_ok() {
      removed += 1;
    }
  }
  removed
}

/// Create the per-run scratch dir under `/tmp` using scsh's
/// `scsh-YYYYMMDD-HHMMSS-utc-run-<skill>` name, suffixing `-2`, `-3`, … in the
/// unlikely event a same-second run of the same skill already took the name.
fn prepare_run_dir(secs: u64, skill: &str) -> Result<PathBuf, String> {
  let base = runtime::run_dir_name(secs, skill);
  for n in 1..=100 {
    let dir = PathBuf::from("/tmp").join(if n == 1 { base.clone() } else { format!("{base}-{n}") });
    match std::fs::create_dir(&dir) {
      Ok(()) => return Ok(dir),
      Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
      Err(e) => return Err(format!("could not create run dir {}: {e}", dir.display())),
    }
  }
  Err("could not create a unique run dir under /tmp".into())
}

/// Full clone (all history, all branches) of the host repo at `root` into the
/// already-created, empty `run_dir`, then materialize every remote branch as a
/// local one so the container sees them all.
fn clone_into(root: &Path, run_dir: &Path, spinner: &ui::screen::Proc) -> Result<(), String> {
  let cmd = runtime::clone_command(&root.to_string_lossy(), &run_dir.to_string_lossy());
  let (ok, last) = spinner.run(&cmd[0], &cmd[1..]).map_err(|e| format!("failed to run git clone: {e}"))?;
  if !ok {
    return Err(match last {
      Some(l) if !l.is_empty() => format!("git clone failed: {l}"),
      _ => "git clone failed".to_string(),
    });
  }
  materialize_branches(run_dir);
  Ok(())
}

/// Best-effort: create a local branch for each `origin/*` branch the clone
/// fetched, so `git branch` in the container lists them all. Failures here never
/// abort the run — the full history is already present either way.
fn materialize_branches(run_dir: &std::path::Path) {
  let current = git_capture(run_dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
  let refs = match git_capture(run_dir, &["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin"]) {
    Some(r) => r,
    None => return,
  };
  for b in runtime::local_branches_to_create(&refs, current.trim()) {
    let _ = Command::new("git")
      .arg("-C")
      .arg(run_dir)
      .args(["branch", "--force", &b, &format!("origin/{b}")])
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status();
  }
}

/// Best-effort: stop a named container, used when a skill's run times out (the
/// `--rm` on the run then removes it). Killing the client process alone leaves a
/// daemon-backed container — e.g. docker — running, so this asks the runtime too.
fn kill_container(runtime: &str, name: &str) {
  let _ = Command::new(runtime)
    .args(["kill", name])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();
}

// ---------------------------------------------------------------------------
// opencode credentials
//
// opencode in the container needs the host's login to talk to a model. The agent
// user's home is the bind-mounted run dir, and opencode reads its auth from
// `$XDG_DATA_HOME/opencode/auth.json` (default `~/.local/share/opencode/…`). So
// scsh copies just that one file (the OAuth token, which keeps itself renewed)
// into each run dir before the run and removes it right after, so the secret
// never lingers in the system temp dir. Opt out with `SCSH_NO_OPENCODE_AUTH=1`.
// ---------------------------------------------------------------------------

/// Whether scsh forwards opencode credentials into runs (on unless opted out).
fn opencode_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_OPENCODE_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh keeps every skill's `/tmp` run-clone instead of cleaning up. By default a
/// successful skill's clone is removed after the run (its result was collected and any commits
/// integrated) while a failed skill's clone is kept for inspection, and stale clones from past
/// runs are swept at startup. Set `SCSH_KEEP_RUNS=1` to keep all clones and skip the sweep.
fn keep_run_dirs() -> bool {
  matches!(std::env::var("SCSH_KEEP_RUNS").ok().as_deref(), Some("1") | Some("true"))
}

/// The opencode `auth.json` path for the given `XDG_DATA_HOME` / `HOME` (pure, so
/// it can be unit-tested). XDG wins when set and non-empty, else `HOME/.local/share`.
fn opencode_auth_in(xdg_data_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
  let base = match xdg_data_home {
    Some(x) if !x.is_empty() => PathBuf::from(x),
    _ => PathBuf::from(home?).join(".local").join("share"),
  };
  Some(base.join("opencode").join("auth.json"))
}

/// The host's opencode `auth.json`, if it exists.
fn host_opencode_auth() -> Option<PathBuf> {
  let path = opencode_auth_in(std::env::var_os("XDG_DATA_HOME").as_deref(), std::env::var_os("HOME").as_deref())?;
  path.is_file().then_some(path)
}

/// Copy `src` (the host auth) into `run_dir`'s opencode data dir, `chmod 600`, and
/// return the destination — where the container's opencode will read it. The data dir lives
/// under the gitignored `tmp/` (matching the image's `XDG_DATA_HOME`), so the forwarded secret
/// never shows as an untracked file in the cloned repo.
fn copy_auth_into(run_dir: &Path, src: &Path) -> Option<PathBuf> {
  let dir = run_dir.join(runtime::AGENT_XDG_DATA_REL).join("opencode");
  std::fs::create_dir_all(&dir).ok()?;
  let dest = dir.join("auth.json");
  std::fs::copy(src, &dest).ok()?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
  }
  Some(dest)
}

/// Forward the host's opencode credentials into `run_dir` for the upcoming run,
/// returning the copied secret's path (so the caller can remove it afterward).
fn forward_opencode_auth(run_dir: &Path) -> Option<PathBuf> {
  copy_auth_into(run_dir, &host_opencode_auth()?)
}

/// Resolve a skill's `env:` specs against the host environment into the
/// `(key, value)` pairs to forward into its container. `Err(message)` when a
/// required variable (`${VAR}`, `$VAR`, or `${VAR:?message}`) is unset — the skill
/// is refused before any work. A `${VAR:-default}` injects the host value or the
/// default; a constant is always forwarded.
fn resolve_env(env: &[config::EnvVar]) -> Result<Vec<(String, String)>, String> {
  use config::EnvRule;
  let mut out = Vec::new();
  for var in env {
    match &var.rule {
      EnvRule::Default { src, default } => {
        let value = std::env::var(src).unwrap_or_else(|_| default.clone());
        out.push((var.key.clone(), value));
      }
      EnvRule::Require { src, message } => match std::env::var(src) {
        Ok(v) => out.push((var.key.clone(), v)),
        Err(_) => {
          return Err(if message.is_empty() { format!("{src} is required but not set") } else { message.clone() });
        }
      },
      EnvRule::Constant(val) => out.push((var.key.clone(), val.clone())),
    }
  }
  Ok(out)
}

/// Require the skill's `result` file in the clone, then copy it back to the same
/// relative path in the host repo, moving any pre-existing file aside to
/// `<name>.bak.YYYYMMDD-HHMMSS-utc` first. Returns the destination path. Pure of
/// terminal output — the caller's spinner / summary does the reporting.
fn collect_skill_result(root: &Path, run_dir: &Path, result: &str, secs: u64) -> Result<String, String> {
  let produced = run_dir.join(result);
  if !produced.is_file() {
    return Err(format!("did not produce its result file '{result}'"));
  }
  let dest = root.join(result);
  if let Some(parent) = dest.parent() {
    std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
  }
  if dest.exists() {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let backup = dest.with_file_name(runtime::backup_name(&name, secs));
    std::fs::rename(&dest, &backup).map_err(|e| format!("could not back up existing {}: {e}", dest.display()))?;
  }
  std::fs::copy(&produced, &dest).map_err(|e| format!("could not copy result to {}: {e}", dest.display()))?;
  Ok(dest.to_string_lossy().into_owned())
}

/// Run `git -C <dir> <args>` and return its trimmed stdout on success.
fn git_capture(dir: &std::path::Path, args: &[&str]) -> Option<String> {
  let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
  out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The host user's numeric UID/GID (via `id -u` / `id -g`), so the container's
/// `agent` user can own the files it writes into the mount. Falls back to
/// 1000:1000 if `id` is unavailable.
fn host_ids() -> (u32, u32) {
  (id_value("-u").unwrap_or(1000), id_value("-g").unwrap_or(1000))
}

fn id_value(flag: &str) -> Option<u32> {
  let out = Command::new("id").arg(flag).output().ok()?;
  out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten()
}

/// Seconds since the Unix epoch (UTC), for run-dir and backup timestamps.
fn now_secs() -> u64 {
  std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Build the image through the live board's build proc (so its output streams into the
/// collapsible build row, timestamped). docker/podman take the in-memory Dockerfile on stdin;
/// Apple's `container` has no stdin build mode, so it gets an ephemeral context dir instead.
fn run_build(
  build: &ui::screen::Proc, runtime_name: &str, tag: &str, dockerfile: &str, uid: u32, gid: u32,
) -> Result<(), (String, i32)> {
  let tz = runtime::host_timezone();
  let started = |e: std::io::Error| (format!("failed to start '{runtime_name}': {e}"), 1);
  let (ok, last) = match runtime::build_method(runtime_name) {
    runtime::BuildMethod::Stdin => {
      let cmd = runtime::build_command_stdin(runtime_name, tag, uid, gid, &tz);
      build.run_with_stdin(&cmd[0], &cmd[1..], dockerfile.as_bytes()).map_err(started)?
    }
    runtime::BuildMethod::ContextDir => {
      let dir = make_temp_dir().map_err(|e| (format!("could not create build context: {e}"), 1))?;
      let path = dir.join(runtime::CONTEXT_DOCKERFILE_NAME);
      if let Err(e) = std::fs::write(&path, dockerfile) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err((format!("could not write Dockerfile to build context: {e}"), 1));
      }
      let cmd = runtime::build_command_context(runtime_name, tag, &dir.to_string_lossy(), uid, gid, &tz);
      let out = build.run(&cmd[0], &cmd[1..]).map_err(started);
      let _ = std::fs::remove_dir_all(&dir); // best-effort cleanup
      out?
    }
  };
  if ok {
    Ok(())
  } else {
    let msg = match last {
      Some(l) if !l.is_empty() => format!("image build failed: {l}"),
      _ => "image build failed".into(),
    };
    Err((msg, 1))
  }
}

fn make_temp_dir() -> std::io::Result<PathBuf> {
  let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
  let dir = std::env::temp_dir().join(format!("scsh-build-{}-{nanos}", std::process::id()));
  std::fs::create_dir_all(&dir)?;
  Ok(dir)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn git_root() -> Result<PathBuf, String> {
  let out = Command::new("git")
    .args(["rev-parse", "--show-toplevel"])
    .output()
    .map_err(|e| format!("failed to run git: {e}"))?;
  if !out.status.success() {
    return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
  }
  Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn ok(msg: &str) {
  println!("{} {msg}", console::style("\u{2713}").green().bold());
}

fn fail(msg: &str) {
  eprintln!("{} {msg}", console::style("\u{2717}").red().bold().for_stderr());
}

fn hint(msg: &str) {
  eprintln!("  {} {msg}", console::style("\u{2192}").cyan().for_stderr());
}

/// A literal command rendered **bold** for an actionable hint, so the thing to type
/// stands out. Honors NO_COLOR and non-TTY output via `console` (plain text then).
fn bold(s: &str) -> console::StyledObject<&str> {
  console::style(s).bold().for_stderr()
}

fn install_git_hint() -> &'static str {
  if cfg!(target_os = "macos") {
    "install it with: brew install git  (or: xcode-select --install)"
  } else {
    "install it with your package manager, e.g.: sudo apt-get install git"
  }
}

fn install_runtime_hint() -> &'static str {
  if cfg!(target_os = "macos") {
    "install Apple 'container' (https://github.com/apple/container), Docker Desktop, or Podman"
  } else {
    "install Docker (https://docs.docker.com/engine/install/) or Podman (https://podman.io)"
  }
}

// --- help styling -----------------------------------------------------------
// All help goes to stdout; `console` auto-strips color when piped or NO_COLOR is
// set, so the text stays the same and tests still match on plain substrings.

/// A bold, cyan section header (`Commands:`, `Usage:`, …) — the "dark color" accent.
fn h_head(s: &str) -> console::StyledObject<&str> {
  console::style(s).cyan().bold()
}

/// Dimmed secondary text (taglines, descriptions, the aliases footer).
fn h_dim(s: &str) -> console::StyledObject<&str> {
  console::style(s).dim()
}

/// One `• name    description` row: a dim bullet, a bold fixed-width name, dim text.
fn help_row(name: &str, desc: &str) {
  println!("  {} {} {}", h_dim("\u{2022}"), console::style(format!("{name:<25}")).bold(), h_dim(desc));
}

fn print_help(topic: HelpTopic) {
  match topic {
    HelpTopic::Overview => print_help_overview(),
    HelpTopic::Config => print_help_config(),
    HelpTopic::Internals => print_help_internals(),
    HelpTopic::Cache => print_help_cache(),
  }
}

/// The default page: a compact, one-line-per-command overview. The detail lives in
/// the two deep-dive topics so it never floods this screen.
fn print_help_overview() {
  println!();
  println!(
    "{} {} {}",
    console::style("scsh").cyan().bold(),
    h_dim(&version_id()),
    console::style("\u{2014} Scoped Skills Helper").bold()
  );
  println!("{}", h_dim("Run a git repo's scoped skills in parallel \u{2014} each in its own ephemeral"));
  println!("{}", h_dim("container, on a clean clone of your repo \u{2014} all from one .scsh.yml."));
  println!();
  println!(
    "{} {} {}",
    h_head("Usage:"),
    console::style("scsh <command> [options]").bold(),
    h_dim("\u{2014} a bare `scsh` prints this help")
  );
  println!();
  println!("{}", h_head("Commands:"));
  help_row("run [profile…]", "Build the image and run the skills in parallel (bare names select profiles).");
  help_row("list   (alias: ls)", "List every skill by profile — result, commits, env (--verbose: + internals).");
  help_row("init-demo-project", "Scaffold + commit a ready-to-run demo project.");
  help_row("installskills [url]", "Install skills — bundled, or a repo's (merging its .scsh.yml).");
  help_row("updateskills [url]", "Reinstall skills, overwriting files — bundled or a repo's.");
  help_row("version", "Print the version (with the build's git hash).");
  help_row("help [topic]", "Show this help, or one of the topics below.");
  println!();
  println!("{}", h_head("More help:"));
  help_row("scsh help .scsh.yml", "The project config file: every field + env syntax.");
  help_row("scsh help internals", "How a run works: preflight, clone, image, results.");
  help_row("scsh help cache", "How results are cached, and when a re-run is a hit.");
  println!();
  println!("{}", h_head("Options:"));
  help_row("[profile…] / --profile <names>", "run only these profiles — bare after `run` (`run a b`) or a comma/semicolon list (`default` = the no-profile skills).");
  help_row("--verbose", "with list, also print the image Dockerfile and exact commands.");
  println!();
  println!("{}", h_dim("`run` bakes a dev toolchain into the image (python3/uv, Go, Rust, gh, aws, gcloud,"));
  println!("{}", h_dim("kubectl, psql, protoc, \u{2026}; no Java) and builds it with this machine's timezone."));
  println!("{}", h_dim("Full list: scsh help internals."));
  println!();
  println!("{} {}", h_dim("Aliases:"), h_dim("--help/-h \u{00b7} --version/-V \u{00b7} --init-demo-project"));
  println!();
}

/// `scsh help .scsh.yml` — the project config file, in full.
fn print_help_config() {
  println!();
  println!("{} {}", h_head(".scsh.yml"), console::style("\u{2014} the project config file").bold());
  println!("{}", h_dim("The whole file is just your skills; scsh owns the container command. The base"));
  println!("{}", h_dim("image is built in (Debian + opencode + a dev toolchain) — no version/project/image header."));
  println!();
  print!(
    "{}",
    r#"  skills:                 # the only top-level key: one or more, keyed by skill name
    add:                  #   the name (matches a .skills/<name>/)
      harness: opencode   #     required; the harness that runs it (only `opencode`)
      model: openai/...   #     optional; the model the harness passes to the tool
      timeout: 600        #     optional; seconds — kill the container & fail if exceeded
      env:                #     optional; host vars to forward (-e) into the container
        - A: ${A}         #       require A — refuse the skill if A is unset
        - B: ${B:-5}      #       forward B, or inject the default 5 when unset
        - X: ${X:?msg}    #       require X, refusing with your message
      profile: extra      #     optional; run only under --profile extra (not by default)
      commits: true       #     optional; bring commits the skill makes back onto your
                          #       branch (rebased; or saved to scsh/incoming/<skill>-…
                          #       if they don't apply cleanly). A real, repeatable side
                          #       effect — run twice and you get the commit twice.
      result: tmp/x.json  #     required; the repo-relative file the skill must write
"#
  );
  println!();
  println!("{}", h_head("Env value syntax"));
  println!("{}", h_dim("  scsh resolves each value on the host, then forwards it (or refuses the skill):"));
  help_row("${VAR} or $VAR", "require VAR; refuse the skill if it is unset.");
  help_row("${VAR:-default}", "forward VAR, or inject `default` when unset (${VAR:-} = empty).");
  help_row("${VAR:?message}", "require VAR; refuse with your `message` if unset.");
  help_row("literal", "a bare value like `A: A` is the literal string \"A\".");
  println!();
  println!();
  println!("{}", h_head("Profiles"));
  println!("{}", h_dim("  No `profile:` = the reserved `default` profile (runs on a bare `scsh run`). A skill"));
  println!("{}", h_dim("  with `profile: X` runs only under `--profile X`; pass a list (`--profile a,b`) to run"));
  println!("{}", h_dim("  several. If every skill is profiled, `scsh run` is a no-op that lists the profiles."));
  println!();
  println!("{}", h_head("Sharing skills (install sources)"));
  println!("{}", h_dim("  When another repo runs `scsh installskills <this-repo>`, scsh installs every skill in"));
  println!("{}", h_dim("  this manifest EXCEPT those marked `autoinstall: false` or named `internal-*` (both"));
  println!("{}", h_dim("  authoring-only), merging the rest into that repo's own .scsh.yml."));
  println!();
  println!("{}", h_dim("The harness runs, inside the container:  opencode -m <model> run \"run skill <name>\""));
  println!();
}

/// `scsh help internals` — how a run actually works, end to end.
fn print_help_internals() {
  println!();
  println!("{} {}", h_head("Internals"), console::style("\u{2014} how a run works").bold());
  println!();
  println!("{}", h_head("Preflight order"));
  println!("{}", h_dim("  A real `run` is repo-hygiene-first and fails in this order; the message names"));
  println!("{}", h_dim("  exactly what's wrong and the one command to fix it."));
  print!(
    r#"    1. git is installed
    2. the current directory is inside a git repository
    3. the working tree is clean       (run only; scsh clones COMMITTED state)
    4. .scsh.yml exists, and matches the schema
    5. /tmp is gitignored               (run only; build scratch + results stay untracked)
    6. a container runtime is available (macOS: container -> docker -> podman;
       otherwise docker -> podman; override with SCSH_RUNTIME=podman)
    7. the runtime's engine is running  (run only; scsh prints how to start it)
    (list runs only the non-run checks: git, repo, config, runtime.)
"#
  );
  println!();
  println!("{}", h_head("How a run works"));
  print!(
    r#"  scsh builds ONE generic image (opencode + a non-root `agent` user whose UID/GID
  match yours), version-checking opencode during the build. Then, for EVERY skill in
  parallel, it makes a fresh full clone of this repo into a /tmp run dir
  (scsh-YYYYMMDD-HHMMSS-utc-run-<skill>, all branches) and runs the skill's harness
  in its own container with that clone bind-mounted at /home/agent/repo (the WORKDIR).
  The clone is mounted UNDER the agent's home, not as it, so the harness's home-dir
  scratch (~/.cache, ~/.config, ~/.npm) stays out of the cloned tree. Files the skill
  writes are owned by you on the host.

  Each skill MUST produce its declared `result` file. Missing after the container
  exits -> that skill fails and the whole invocation exits non-zero; otherwise scsh
  copies the result back into your repo, moving any existing file aside to
  <name>.bak.YYYYMMDD-HHMMSS-utc. All skills run regardless, so one run reports
  every skill's outcome.

  So the container's opencode can reach a model, scsh copies your host opencode auth
  (~/.local/share/opencode/auth.json) into the run's gitignored tmp/ (as XDG_DATA_HOME)
  for its duration and removes it afterward (opt out: SCSH_NO_OPENCODE_AUTH=1). Every
  line of harness output is teed to <run_dir>/tmp/scsh-run.log for inspection.

  The Dockerfile is generated in memory (streamed to the builder's stdin), and your
  repository is modified only by the result copies (into the gitignored tmp/).

  Cleanup: a skill's container is --rm, and its /tmp clone is host-side scratch. After a
  SUCCESSFUL skill scsh removes that clone; a FAILED skill's clone is kept for inspection
  (its path is printed). Stale clones from past runs (>24h old) are swept at the next run's
  start. Keep every clone with SCSH_KEEP_RUNS=1 (also skips the sweep).

  The live board: on a terminal the build and every skill are drawn as collapsible rows,
  inline in the normal buffer (no alternate screen, so your scrollback keeps working). Each row
  carries a [Ctrl+N] label on the left: PRESS Ctrl+1 … Ctrl+9 to expand/collapse it (scsh turns
  on the terminal's keyboard-enhancement protocol so every Ctrl+digit works; without it, the
  plain digit toggles — or click the row if the mouse is forwarded). Expanding shows the proc's
  output, each line stamped with its time relative to that proc's start. SCROLL with the wheel,
  ↑↓, PgUp/PgDn or Home/End (e/c expand/collapse all; Ctrl-C aborts). On finish scsh wipes the
  live region and leaves a compact ✓/✗ summary. Off a TTY it falls back to plain ▶ / ✓ / ✗ lines.
"#
  );
  println!();
  println!("{}", h_head("What's in the image"));
  print!(
    r#"  A glibc Debian-slim base, baked with a broad dev/CLI toolchain so skills work with
  no setup step. Built once, then cached and reused (the first run does the build):
    languages/build  python3 (+ uv), Go, Rust (cargo), C/C++ (gcc/g++/make/cmake),
                     perl, gawk, node (+ opencode, the harness)
    data/CLI         jq, yq, ripgrep, shellcheck, git (+ git-lfs), gh, sqlite3,
                     psql, protoc, curl/wget, tar/gzip/xz/zip/unzip, patch, tree
    cloud            aws (v2), gcloud + gsutil, kubectl
    networking       ping, traceroute, dig/nslookup, nc, ss/ip, whois, socat
  Java is intentionally NOT installed (nothing here is JVM; a JDK adds ~300 MB).
  The image is built with the TIMEZONE OF THE MACHINE BUILDING IT (scsh passes the
  host's TZ as a build arg), so timestamps a skill produces match your machine.
  It is platform-agnostic: the same Dockerfile builds on x86_64 and arm64 (arch is
  resolved at build time, no hardcoded-arch downloads).
"#
  );
  println!();
}

/// `scsh help cache` — the content-addressed result cache.
fn print_help_cache() {
  println!();
  println!("{} {}", h_head("Cache"), console::style("\u{2014} content-addressed skill results").bold());
  println!();
  println!("{}", h_dim("scsh caches each skill's result and reuses it when nothing that matters changed —"));
  println!("{}", h_dim("a cache hit returns the result instantly, with no clone, no container, no model call."));
  println!();
  println!("{}", h_head("The cache key"));
  print!(
    "{}",
    r#"  Before running a skill, scsh hashes (sha256) a deterministic blob of:
    • the repo's committed content (the git HEAD tree),
    • the skill's own files (SKILL.md + scripts), and
    • the resolved environment forwarded to the skill (sorted).
  Same commit + same skill + same env  =>  same key  =>  a hit. Change any of them
  (edit a file, pass A=9, tweak the skill) and the key changes => a miss.
"#
  );
  println!();
  println!("{}", h_head("Where it lives"));
  print!(
    "{}",
    r#"  Under the repo's gitignored tmp/: tmp/.sccache/<sha256>.json, and nowhere else. Each
  entry holds the skill's result AND, for a commit-enabled skill, the commits it made
  (journaled as a git patch). On a hit scsh restores the result file, prints it with
  "(cached)", and replays any journaled commits; on a miss it runs and stores both.
"#
  );
  println!();
  println!("{}", h_head("Commits are journaled and replayed (a hit reproduces them)"));
  print!(
    "{}",
    r#"  A commit-enabled skill (commits: true) changes the repo when it commits, so the very
  next run sees a NEW HEAD tree => a different key => a miss => it runs (and commits) again.
  But the commits ARE journaled in the cache. Revert to the same committed state (e.g.
  git reset --hard to before the skill's commit) and run again => the key matches => a HIT:
  scsh restores the result AND replays the journaled commits, so the commit reappears on top.
  A hit reproduces the full side effect, not just the result. (If a replay can't apply
  cleanly, scsh saves the patch under tmp/.sccache/ and leaves your branch alone.)
"#
  );
  println!();
  println!("{}", h_head("The author you'll recognize (a tripwire)"));
  print!(
    "{}",
    r#"  scsh stamps the commits a skill makes with a deliberately unmistakable author —
  dkorolev-neon-elon-bot <dmitry.korolev+elon-presley@gmail.com> (yes, a neon-cyberpunk
  Elon). It is never a real contributor. These commits are LOCAL-ONLY by design: scsh
  rebases them onto your branch, it never pushes. So if that face ever shows up in a code
  review or a pushed commit list, that's your signal — you pushed something you shouldn't
  have. Go check.
"#
  );
  println!();
}
