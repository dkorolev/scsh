//! scsh — Scoped Skills Helper.
//!
//! Preflight a git repository (git → repo → `.scsh.yml` present → schema-valid →
//! a container runtime), then build one in-memory image and run the project's
//! scoped skills — all of them, in parallel, each in its own ephemeral container
//! under its configured harness.

mod config;
mod daemon;
mod failure;
mod json;
mod runtime;
mod sha1;
mod sha256;
mod stats;
mod ui;
mod version;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use config::ResolvedInvocation;
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
    Mode::InitDemo => init_demo(),
    Mode::InstallSkills => install_skills(false, &cli.sources),
    Mode::UpdateSkills => install_skills(true, &cli.sources),
    Mode::List => {
      // `--json` is a runtime-free, machine-readable listing (just git + a valid .scsh.yml);
      // the human listing goes through the full preflight like a run does.
      if cli.json {
        list_profiles_json()
      } else {
        preflight_then(Action::List, profile, cli.verbose)
      }
    }
    Mode::CheckProfile => check_profile_cmd(profile),
    Mode::Run => preflight_then(Action::Run, profile, cli.verbose),
    // Hidden: a self-contained demo of the live board (no container/model needed), used by the
    // feature's demo + PTY test. `--frames` dumps deterministic plain frames; otherwise it runs
    // the real interactive board over a few scripted subprocesses.
    Mode::UiDemo { frames } => ui::demo::run(frames),
    Mode::Daemon { action } => daemon_cmd(action),
    Mode::DaemonServe { mode, port } => daemon_serve(mode, port),
    Mode::Failures => failures_cmd(&cli.failures),
    Mode::Stats => stats_cmd(&cli.failures, profile),
    Mode::Prune => prune_cmd(cli.prune_now),
  }
}

#[derive(Clone, Copy)]
enum Mode {
  Help(HelpTopic),
  Version,
  InitDemo,
  InstallSkills,
  UpdateSkills,
  List,
  CheckProfile,
  Run,
  UiDemo {
    frames: bool,
  },
  Daemon {
    action: DaemonAction,
  },
  /// Hidden: the long-lived HTTP server process.
  DaemonServe {
    mode: daemon::DaemonMode,
    port: u16,
  },
  /// Browse the failure log (`scsh failures`), with filters and `--stats`.
  Failures,
  /// Browse durable run statistics (`scsh stats`): durations and workload per route.
  Stats,
  /// Show the run-dir prune queue; `--now` forces a janitor pass.
  Prune,
}

#[derive(Clone, Copy)]
enum DaemonAction {
  Start,
  Stop,
  Restart,
  Status,
}

/// Which help page to print. The default (`scsh help` / a bare `scsh`) is a compact
/// overview of the commands; the deep-dive topics keep their detail OUT of the default
/// output (`scsh help run`, `scsh help .scsh.yml`, `scsh help internals`, `scsh help cache`).
#[derive(Clone, Copy)]
enum HelpTopic {
  Overview,
  Run,
  Config,
  Internals,
  Cache,
}

fn version_id() -> String {
  version::display()
}

enum Action {
  List,
  Run,
}

/// A parsed command line: one command, plus the profiles that select which skills run (bare
/// positional names and/or `--profile` for `run`; the one profile name for `check-profile`),
/// the source repos (git URLs/paths for `installskills` / `updateskills` — one or more,
/// installed in order), and the `list` output flags (`--verbose`, `--json`).
struct Cli {
  mode: Mode,
  profile: Option<String>,
  sources: Vec<String>,
  verbose: bool,
  json: bool,
  failures: FailuresOpts,
  prune_now: bool,
}

/// Filters and output flags shared by `scsh failures` and `scsh stats`.
#[derive(Default)]
struct FailuresOpts {
  session: Option<String>,
  skill: Option<String>,
  reason: Option<String>,
  stats: bool,
  /// How many trailing events/rows to show (`--last N`; `--last 0` = all; default 50).
  last: Option<usize>,
  /// `scsh stats` route filters.
  harness: Option<String>,
  model: Option<String>,
  /// `scsh stats --raw`: print individual rows instead of aggregates.
  raw: bool,
}

/// Parse cargo-style subcommands. The default (no command) is `help`, so a bare
/// `scsh` is safe and self-explanatory; `run` is the explicit "do it" command.
/// The old `--init-demo-project` / `--help` / `--version`
/// flags keep working as aliases.
fn parse_cli(args: &[String]) -> Result<Cli, String> {
  let mut mode: Option<Mode> = None;
  let mut profiles: Vec<String> = Vec::new();
  let mut sources: Vec<String> = Vec::new();
  let mut verbose = false;
  let mut json = false;
  let mut frames = false;
  let mut failures = FailuresOpts::default();
  let mut prune_now = false;
  let mut i = 0;
  while i < args.len() {
    let m = match args[i].as_str() {
      "help" | "-h" | "--help" => {
        // An optional next token selects a deep-dive topic; otherwise the overview.
        let topic = match args.get(i + 1).map(|s| s.as_str()) {
          Some("run") => {
            i += 1;
            HelpTopic::Run
          }
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
            return Err(format!("unknown help topic '{other}' (topics: run, .scsh.yml, internals, cache)"));
          }
          _ => HelpTopic::Overview,
        };
        Some(Mode::Help(topic))
      }
      "version" | "-V" | "--version" => Some(Mode::Version),
      "run" => Some(Mode::Run),
      "list" | "ls" => Some(Mode::List),
      // `check-profile <name>`: a runtime-free existence check for scripts — the next token is
      // the profile name to test (exit 0 iff it exists with >=1 skill).
      "check-profile" => {
        i += 1;
        let name = args.get(i).ok_or("check-profile needs a profile name, e.g. scsh check-profile multiply")?;
        if name.trim().is_empty() {
          return Err("check-profile name must not be empty".into());
        }
        profiles.push(name.clone());
        Some(Mode::CheckProfile)
      }
      // Hidden dev command: demo the live board with no container/model (see `ui::demo`).
      "__ui-demo" => Some(Mode::UiDemo { frames: false }),
      "--frames" => {
        frames = true;
        None
      }
      "init-demo-project" | "init" | "--init-demo-project" => Some(Mode::InitDemo),
      // `installskills [<git-url>…]` / `updateskills [<git-url>…]`: positional source repos
      // (one or more) install skills from those repos, in order, instead of scsh's bundled one.
      "installskills" => Some(Mode::InstallSkills),
      "updateskills" => Some(Mode::UpdateSkills),
      // `failures [--session S] [--skill NAME] [--reason CODE] [--last N] [--stats]`:
      // browse the append-only failure log (see `scsh run`'s "failure log:" hint).
      "failures" => Some(Mode::Failures),
      // `stats [--skill NAME] [--profile P] [--harness H] [--model M] [--raw] [--last N]`:
      // durations and workload sizes per skill and harness·model route (~/.scsh/stats.jsonl).
      "stats" => Some(Mode::Stats),
      "--session" | "--skill" | "--reason" | "--harness" | "--model" => {
        let flag = args[i].clone();
        i += 1;
        let value = args.get(i).ok_or_else(|| format!("{flag} needs a value"))?.clone();
        match flag.as_str() {
          "--session" => failures.session = Some(value),
          "--skill" => failures.skill = Some(value),
          "--harness" => failures.harness = Some(value),
          "--model" => failures.model = Some(value),
          _ => failures.reason = Some(value),
        }
        None
      }
      "--stats" => {
        failures.stats = true;
        None
      }
      "--raw" => {
        failures.raw = true;
        None
      }
      "--last" => {
        i += 1;
        let n = args.get(i).ok_or("--last needs a number (0 = all)")?;
        failures.last = Some(n.parse().map_err(|_| format!("bad --last value '{n}'"))?);
        None
      }
      // `prune [--now]`: show the daemon's run-dir cleanup queue, or force a pass now.
      "prune" => Some(Mode::Prune),
      "--now" => {
        prune_now = true;
        None
      }
      "daemon" => {
        i += 1;
        let sub = args.get(i).ok_or("daemon needs a subcommand: start, stop, restart, or status")?;
        let action = match sub.as_str() {
          "start" => DaemonAction::Start,
          "stop" => DaemonAction::Stop,
          "restart" => DaemonAction::Restart,
          "status" => DaemonAction::Status,
          other => return Err(format!("unknown daemon subcommand '{other}' (try: start, stop, restart, status)")),
        };
        Some(Mode::Daemon { action })
      }
      "__daemon-serve" => {
        let mut mode = daemon::DaemonMode::Ephemeral;
        let mut port = daemon::daemon_port();
        loop {
          i += 1;
          match args.get(i).map(|s| s.as_str()) {
            None => break,
            Some("--mode") => {
              i += 1;
              let m = args.get(i).ok_or("__daemon-serve --mode needs persistent or ephemeral")?;
              mode = daemon::DaemonMode::parse(m).ok_or_else(|| format!("bad daemon mode '{m}'"))?;
            }
            Some("--port") => {
              i += 1;
              let p = args.get(i).ok_or("__daemon-serve --port needs a number")?;
              port = p.parse().map_err(|_| format!("bad port '{p}'"))?;
            }
            Some(other) if other.starts_with('-') => {
              return Err(format!("unknown __daemon-serve option '{other}'"));
            }
            Some(_) => break,
          }
        }
        Some(Mode::DaemonServe { mode, port })
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
      "--verbose" | "-v" => {
        verbose = true;
        None
      }
      "--json" => {
        json = true;
        None
      }
      // After `run`, a bare token is a profile name: `scsh run a b` == `scsh run --profile a,b`.
      // (A `-`-prefixed token is still an unknown flag, and bare tokens before a command — or
      // after any non-`run` command — remain errors.)
      other if matches!(mode, Some(Mode::Run)) && !other.starts_with('-') => {
        profiles.push(other.to_string());
        None
      }
      // After `installskills`/`updateskills`, each bare token is a source repo — they're
      // installed in order, as if the command were run once per repo.
      other if matches!(mode, Some(Mode::InstallSkills | Mode::UpdateSkills)) && !other.starts_with('-') => {
        sources.push(other.to_string());
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
  // Positional profiles and any `--profile` values combine into one comma-joined spec
  // (requested_profiles splits on `,`/`;`), so `run a b`, `run --profile a,b`, and
  // `run --profile a b` are all equivalent.
  let profile = if profiles.is_empty() { None } else { Some(profiles.join(",")) };
  // `check-profile` carries its single profile name in the same field; `stats` filters by it.
  if profile.is_some() && !matches!(mode, Mode::Run | Mode::CheckProfile | Mode::Stats) {
    return Err(
      "profiles only apply to 'run' and 'stats' (e.g. `scsh run code-review` or `scsh stats --profile code-review`)"
        .into(),
    );
  }
  if !sources.is_empty() && !matches!(mode, Mode::InstallSkills | Mode::UpdateSkills) {
    return Err("a skills source (git URL) only applies to 'installskills' or 'updateskills'".into());
  }
  if verbose && !matches!(mode, Mode::List) {
    return Err("--verbose only applies to 'list'".into());
  }
  if json && !matches!(mode, Mode::List) {
    return Err("--json only applies to 'list' (e.g. `scsh list --json`)".into());
  }
  if (failures.reason.is_some() || failures.stats) && !matches!(mode, Mode::Failures) {
    return Err("--reason/--stats only apply to 'failures'".into());
  }
  if (failures.harness.is_some() || failures.model.is_some() || failures.raw) && !matches!(mode, Mode::Stats) {
    return Err("--harness/--model/--raw only apply to 'stats'".into());
  }
  let shared_query_flags = failures.session.is_some() || failures.skill.is_some() || failures.last.is_some();
  if shared_query_flags && !matches!(mode, Mode::Failures | Mode::Stats) {
    return Err("--session/--skill/--last only apply to 'failures' or 'stats'".into());
  }
  if prune_now && !matches!(mode, Mode::Prune) {
    return Err("--now only applies to 'prune' (e.g. `scsh prune --now`)".into());
  }
  Ok(Cli { mode, profile, sources, verbose, json, failures, prune_now })
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

/// Invocations selected for a run after expanding matrix skills. Those whose profile is in
/// the requested set run; a skill with no `profile:` belongs to the reserved `default` profile.
fn select_invocations(cfg: &config::Config, profile: Option<&str>) -> Vec<ResolvedInvocation> {
  let want = requested_profiles(profile);
  config::expand_invocations(cfg)
    .into_iter()
    .filter(|s| want.contains(s.profile.as_deref().unwrap_or("default")))
    .collect()
}

/// The distinct profile names across expanded invocations, in first-seen order.
fn declared_profiles(cfg: &config::Config) -> Vec<String> {
  let mut out = Vec::new();
  for inv in config::expand_invocations(cfg) {
    let p = inv.profile.as_deref().unwrap_or("default").to_string();
    if !out.contains(&p) {
      out.push(p);
    }
  }
  out
}

// ---------------------------------------------------------------------------
// Preflight + actions
// ---------------------------------------------------------------------------

fn preflight_then(action: Action, profile: Option<&str>, verbose: bool) -> i32 {
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
      fail(
        "working tree has uncommitted changes — scsh runs a clone of committed state, \
so they would not be in the container",
      );
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
    Action::List => {
      ok(&preflight_summary(&root, &cfg, &rt));
      list_skills(&cfg, &rt, &root, verbose)
    }
    Action::Run => {
      // Every requested --profile must be `default` (the no-profile skills) or a profile
      // this config declares.
      if profile.is_some() {
        let declared = declared_profiles(&cfg);
        let unknown: Vec<String> =
          requested_profiles(profile).into_iter().filter(|p| p != "default" && !declared.contains(p)).collect();
        if !unknown.is_empty() {
          fail(&format!("unknown profile{}: {}", plural(unknown.len()), unknown.join(", ")));
          let mut avail = vec!["default".to_string()];
          avail.extend(declared.iter().map(|s| s.to_string()));
          hint(&format!("available: {} (see them with: scsh list)", avail.join(", ")));
          return 1;
        }
      }
      let selected = select_invocations(&cfg, profile);
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
      // Skip skills whose harness or explicit opencode model is unavailable; fail only when none remain.
      let model_probe = runtime::OpencodeModelProbe::for_selected(&selected);
      let mut runnable: Vec<&ResolvedInvocation> = Vec::new();
      for skill in &selected {
        if let Err(msg) = runtime::check_skill_host(skill.harness, skill.model.as_deref(), &model_probe) {
          warn(&format!("skipping '{}' — {msg}", skill.name));
          continue;
        }
        let skill_md = root.join(".skills").join(&skill.skill_source).join("SKILL.md");
        if !skill_md.is_file() {
          fail(&format!("skill source missing: .skills/{}/SKILL.md (invocation '{}')", skill.skill_source, skill.name));
          return 1;
        }
        runnable.push(skill);
      }
      if runnable.is_empty() {
        fail("no skills to run — every selected skill was skipped (harness or model unavailable on this host)");
        hint("see DEMO.md step 1 — probe add-opencode-gpt-5.4-mini-fast and add-claude-sonnet-4-6");
        return 1;
      }
      build_and_run(&rt, &root, &runnable, profile)
    }
  }
}

/// Compact one-line preflight summary for `list` (no run-only guards).
fn preflight_summary(root: &Path, cfg: &config::Config, rt: &Runtime) -> String {
  let names = cfg.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ");
  let n = cfg.skills.len();
  format!(
    "git · repo {} · .scsh.yml valid ({n} skill{}: {names}) · using {}",
    display_path(root),
    if n == 1 { "" } else { "s" },
    backend_name(&rt.name)
  )
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

/// `scsh list` / `scsh ls` — the inventory: every skill grouped by profile (the reserved
/// `default` profile is the skills with no `profile:`), each with its result file, commit
/// flag, and the env it needs. `--verbose` additionally prints the generated Dockerfile and
/// the exact per-skill build/run commands.
fn list_skills(cfg: &config::Config, rt: &Runtime, root: &std::path::Path, verbose: bool) -> i32 {
  let expanded = config::expand_invocations(cfg);
  println!();
  println!(
    "{} {}",
    h_head("Profiles & skills"),
    h_dim(&format!(
      "\u{2014} {} invocation{} · run one with `scsh run --profile <name>`",
      expanded.len(),
      plural(expanded.len())
    ))
  );
  let mut groups: Vec<(String, Vec<&ResolvedInvocation>)> =
    vec![("default".to_string(), expanded.iter().filter(|s| s.profile.is_none()).collect())];
  for p in declared_profiles(cfg) {
    if p == "default" {
      continue;
    }
    groups.push((p.clone(), expanded.iter().filter(|s| s.profile.as_deref() == Some(p.as_str())).collect()));
  }
  for (name, members) in &groups {
    if members.is_empty() {
      let note = if name == "default" { "\u{2014} empty (a bare `scsh run` is a no-op)" } else { "\u{2014} empty" };
      println!("  {} {}", h_head(&format!("{name} (0)")), h_dim(note));
      continue;
    }
    let how = if name == "default" { "scsh run".to_string() } else { format!("scsh run --profile {name}") };
    println!("  {} {}", h_head(&format!("{name} ({})", members.len())), h_dim(&format!("\u{2014} {how}")));
    for s in members {
      let mut notes = String::new();
      if s.commits {
        notes.push_str("  \u{b7} commits back");
      }
      let env: Vec<&str> = s.env.iter().map(|e| e.key.as_str()).collect();
      if !env.is_empty() {
        notes.push_str(&format!("  \u{b7} env: {}", env.join(", ")));
      }
      help_row(&s.name, &format!("\u{2192} {}{notes}", s.result));
    }
  }

  if verbose {
    let skills = &expanded[..];
    let (uid, gid) = host_ids();
    let df = runtime::dockerfile();
    let mut harnesses: std::collections::BTreeSet<config::Harness> = std::collections::BTreeSet::new();
    for s in skills.iter() {
      harnesses.insert(s.harness);
    }
    println!("\n{}", h_head("Images"));
    println!("{}", h_dim("--- generated Dockerfile (in memory; shared base + per-harness targets) ---"));
    print!("{df}");
    let host_tz = runtime::host_timezone();
    let base_fp = runtime::base_image_fingerprint(&df, uid, gid, &host_tz);
    println!("--- build {} first (shared toolchain; agent uid={uid} gid={gid}) ---", runtime::BASE_IMAGE_TARGET);
    print_build_command(
      &rt.name,
      runtime::BASE_IMAGE_TAG,
      runtime::BASE_IMAGE_TARGET,
      &df,
      uid,
      gid,
      &host_tz,
      &base_fp,
    );
    let specs: Vec<runtime::ImageBuildSpec> =
      harnesses.iter().map(|h| runtime::image_build_spec(*h, &df, uid, gid, &host_tz)).collect();
    for spec in &specs {
      println!("--- build {} (harness layer on top of {}) ---", spec.target, runtime::BASE_IMAGE_TARGET);
      print_build_command(&rt.name, &spec.tag, &spec.target, &df, uid, gid, &host_tz, &spec.fingerprint);
    }
    println!("\n{}", h_head("Per-skill commands"));
    for skill in skills {
      let name = runtime::run_dir_name(now_secs(), &skill.name, &rt.name);
      let run_dir = format!("/tmp/{name}");
      let tag = runtime::image_tag(skill.harness);
      let cmd = runtime::harness_command(
        skill.harness,
        skill.model.as_deref(),
        skill.effort.as_deref(),
        &skill.skill_source,
        &skill.result,
        skill.terminal,
      );
      let model = skill.model.as_deref().unwrap_or("(harness default)");
      let timeout = skill.timeout.map(|t| format!("{t}s")).unwrap_or_else(|| "none".into());
      println!(
        "\n[{}]  skill={}  harness={}  model={model}  timeout={timeout}",
        skill.name,
        skill.skill_source,
        skill.harness.as_str()
      );
      if runtime::uses_git_transport(&rt.name) {
        println!("  push:  git push {run_dir}/{} HEAD refs/remotes/origin/*", runtime::TRANSPORT_BARE);
        println!(
          "  run:   container clones git://<gateway>:<port>/{} (gateway from ip route; port in SCSH_GIT_PORT)",
          runtime::TRANSPORT_BARE
        );
      } else {
        println!("  clone: {}", runtime::shell_join(&runtime::clone_command(&root.to_string_lossy(), &run_dir)));
      }
      match resolve_env(&skill.env) {
        Ok(env) => {
          let vols: Vec<(String, String)> = runtime::harness_volumes(skill.harness);
          let vol_refs: Vec<(&str, &str)> = vols.iter().map(|(h, m)| (h.as_str(), m.as_str())).collect();
          let repo_mount = if runtime::uses_git_transport(&rt.name) {
            runtime::RepoMountMode::TmpOnly
          } else {
            runtime::RepoMountMode::Full
          };
          let run = runtime::run_command(&rt.name, &tag, &run_dir, &name, &env, &vol_refs, &cmd, repo_mount);
          println!("  run:   {}", runtime::shell_join(&run));
        }
        Err(message) => println!("  run:   (skill would be REFUSED before running — {message})"),
      }
      println!("  after: require '{}', then copy it back into the repo (backing up any existing file)", skill.result);
    }
  } else {
    println!("{}", h_dim("  run `scsh list --verbose` to also see the image Dockerfile and exact commands"));
  }
  println!();
  0
}

// ---------------------------------------------------------------------------
// Programmatic profile inspection (runtime-free): `list --json` + `check-profile`
//
// These let another tool discover and gate on profiles without scraping the human
// listing and without a container runtime — they only need git, a repo, and a
// schema-valid .scsh.yml. Errors go to stderr (✗/→) so stdout stays machine-clean.
// ---------------------------------------------------------------------------

/// Load and schema-validate the repo's `.scsh.yml` for the read-only inspection commands —
/// the same git → repo → present → valid chain as a run's preflight, but WITHOUT the
/// container-runtime/engine checks, so profiles can be queried on any machine. On failure it
/// reports the problem and returns the process exit code; stdout is left untouched.
fn load_config_for_inspection() -> Result<config::Config, i32> {
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return Err(1);
  }
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return Err(1);
    }
  };
  let cfg_path = root.join(".scsh.yml");
  if !cfg_path.is_file() {
    fail(".scsh.yml not found — this repository isn't set up for scsh yet");
    hint(&format!("get a ready-to-run project in one command: {}", bold("scsh init-demo-project")));
    return Err(1);
  }
  let src = match std::fs::read_to_string(&cfg_path) {
    Ok(s) => s,
    Err(e) => {
      fail(&format!("could not read .scsh.yml: {e}"));
      return Err(1);
    }
  };
  match config::validate(&src) {
    Ok(cfg) => Ok(cfg),
    Err(errs) => {
      let n = errs.len();
      fail(&format!(".scsh.yml does not match the schema ({n} problem{})", if n == 1 { "" } else { "s" }));
      for e in &errs {
        hint(e);
      }
      Err(1)
    }
  }
}

/// The config's profiles as `(name, skill-names)`: the reserved `default` profile (the
/// no-`profile:` skills) first, then each declared profile in first-seen order.
fn profile_groups(cfg: &config::Config) -> Vec<(String, Vec<String>)> {
  let expanded = config::expand_invocations(cfg);
  let mut groups: Vec<(String, Vec<String>)> =
    vec![("default".to_string(), expanded.iter().filter(|s| s.profile.is_none()).map(|s| s.name.clone()).collect())];
  for p in declared_profiles(cfg) {
    if p == "default" {
      continue;
    }
    let members =
      expanded.iter().filter(|s| s.profile.as_deref() == Some(p.as_str())).map(|s| s.name.clone()).collect();
    groups.push((p, members));
  }
  groups
}

/// `scsh list --json` — every profile and its skills as machine-readable JSON on stdout, so
/// another tool can discover them without scraping the human listing (or needing a runtime).
/// The reserved `default` profile is always present (possibly empty); every other profile
/// listed has at least one skill. Stable shape:
/// `{"profiles":[{"name":"default","skills":["add"]}, …]}`.
fn list_profiles_json() -> i32 {
  let cfg = match load_config_for_inspection() {
    Ok(c) => c,
    Err(code) => return code,
  };
  let groups = profile_groups(&cfg);
  let mut out = String::from("{\n  \"profiles\": [\n");
  for (i, (name, skills)) in groups.iter().enumerate() {
    let names = skills.iter().map(|s| json::quote(s)).collect::<Vec<_>>().join(", ");
    out.push_str(&format!("    {{ \"name\": {}, \"skills\": [{}] }}", json::quote(name), names));
    out.push_str(if i + 1 < groups.len() { ",\n" } else { "\n" });
  }
  out.push_str("  ]\n}");
  println!("{out}");
  0
}

/// `scsh check-profile <name>` — a runtime-free existence check for scripts. Exit 0 iff the
/// profile exists AND has at least one skill (so a caller can gate on it directly); non-zero
/// otherwise. The reserved `default` profile "exists" only when some skill has no `profile:`.
/// Prints a one-line ✓/✗ — the exit code is the contract, so redirect it when scripting.
fn check_profile_cmd(profile: Option<&str>) -> i32 {
  let name = match profile {
    Some(p) => p,
    None => {
      fail("check-profile needs a profile name, e.g. scsh check-profile multiply");
      return 2;
    }
  };
  let cfg = match load_config_for_inspection() {
    Ok(c) => c,
    Err(code) => return code,
  };
  let count = select_invocations(&cfg, Some(name)).len();
  if count > 0 {
    ok(&format!("profile '{name}' has {count} skill{}", plural(count)));
    return 0;
  }
  if name == "default" || declared_profiles(&cfg).iter().any(|p| p == name) {
    fail(&format!("profile '{name}' exists but has no skills"));
  } else {
    fail(&format!("no such profile '{name}'"));
    let mut avail = declared_profiles(&cfg);
    if !avail.iter().any(|p| p == "default") {
      avail.insert(0, "default".to_string());
    }
    hint(&format!("available: {}", avail.join(", ")));
  }
  1
}

fn daemon_cmd(action: DaemonAction) -> i32 {
  match action {
    DaemonAction::Start => match daemon::start_persistent() {
      Ok(()) => {
        ok(&format!("session browser daemon listening on {}", daemon::base_url(daemon::daemon_port())));
        0
      }
      Err(e) => {
        fail(&format!("could not start daemon: {e}"));
        hint("→ check SCSH_DAEMON_PORT and whether another process is already listening on that port");
        1
      }
    },
    DaemonAction::Stop => match daemon::stop() {
      Ok(true) => {
        ok("session browser daemon stopped");
        0
      }
      Ok(false) => {
        fail("session browser daemon is not running");
        hint("→ start it with: scsh daemon start");
        1
      }
      Err(e) => {
        fail(&format!("could not stop daemon: {e}"));
        hint("→ check SCSH_DAEMON_PORT and stale files under $TMPDIR/scsh-daemon/");
        1
      }
    },
    DaemonAction::Restart => {
      let _ = daemon::stop();
      match daemon::start_persistent() {
        Ok(()) => {
          ok(&format!("session browser daemon restarted on {}", daemon::base_url(daemon::daemon_port())));
          0
        }
        Err(e) => {
          fail(&format!("could not restart daemon: {e}"));
          hint("→ check SCSH_DAEMON_PORT and whether another process is listening on that port");
          1
        }
      }
    }
    DaemonAction::Status => {
      let port = daemon::daemon_port();
      if daemon::Client::daemon_alive() {
        if let Some(pid) = daemon::read_live_pid(port) {
          ok(&format!("session browser daemon running (pid {pid}) on {}", daemon::base_url(port)));
        } else {
          ok(&format!("session browser daemon responding on {}", daemon::base_url(port)));
        }
        0
      } else if let Some(pid) = daemon::read_live_pid(port) {
        fail(&format!("session browser daemon pid {pid} exists but is not responding on {}", daemon::base_url(port)));
        hint("→ recover with: scsh daemon restart");
        1
      } else {
        fail("session browser daemon is not running");
        hint("→ start it with: scsh daemon start");
        1
      }
    }
  }
}

fn daemon_serve(mode: daemon::DaemonMode, port: u16) -> i32 {
  let server = daemon::Server::new(mode, port);
  match server.run() {
    Ok(()) => 0,
    Err(e) => {
      fail(&format!("session browser daemon exited: {e}"));
      hint("→ check SCSH_DAEMON_PORT and logs from the child process");
      1
    }
  }
}

/// `scsh failures`: render the JSONL failure log, filtered and optionally aggregated.
fn failures_cmd(opts: &FailuresOpts) -> i32 {
  let mut events = failure::read_events();
  if let Some(s) = &opts.session {
    events.retain(|e| e.session.as_deref() == Some(s.as_str()));
  }
  if let Some(s) = &opts.skill {
    events.retain(|e| e.skill.as_deref() == Some(s.as_str()));
  }
  if let Some(r) = &opts.reason {
    events.retain(|e| e.reason == *r);
  }
  if events.is_empty() {
    println!("no recorded failures match");
    hint(&format!("failure log: {}", failure::log_path().display()));
    return 0;
  }
  if opts.stats {
    print_failure_stats(&events);
    return 0;
  }
  let keep = match opts.last {
    Some(0) => events.len(),
    Some(n) => n,
    None => 50,
  };
  let start = events.len().saturating_sub(keep);
  if start > 0 {
    println!("… {start} earlier event(s) hidden — rerun with --last 0 for all");
  }
  for e in &events[start..] {
    print_failure_event(e);
  }
  0
}

fn print_failure_event(e: &failure::FailureEvent) {
  let when = runtime::format_utc_timestamp(e.ts);
  if e.kind == "run_summary" {
    let profile = e.profile.as_deref().unwrap_or("(no profile)");
    let session = e.session.as_deref().unwrap_or("?");
    println!(
      "{when}  run failed: {}/{} skills (profile {profile}, session {session})",
      e.failed.unwrap_or(0),
      e.total.unwrap_or(0)
    );
    return;
  }
  let mut parts = Vec::new();
  if let Some(s) = &e.session {
    parts.push(format!("session={s}"));
  }
  if let Some(s) = &e.skill {
    parts.push(format!("skill={s}"));
  }
  if let Some(s) = &e.subject {
    parts.push(format!("proc={s}"));
  }
  if let Some(h) = &e.harness {
    let model = e.model.as_deref().unwrap_or("(harness default)");
    parts.push(format!("route={h}·{model}"));
  }
  let verb = if e.kind == "retry" { "retried" } else { "failed" };
  println!("{when}  [{}] {verb}  {}", e.reason, parts.join(" "));
  if let Some(d) = &e.detail {
    for line in d.lines() {
      println!("    {line}");
    }
  }
}

/// `scsh failures --stats`: failures and retries per harness·model route, then per reason.
fn print_failure_stats(events: &[failure::FailureEvent]) {
  use std::collections::BTreeMap;
  // Route → (failure count, retry count, reason → count). Only events that carry a route.
  let mut routes: BTreeMap<String, (usize, usize, BTreeMap<String, usize>)> = BTreeMap::new();
  let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
  for e in events {
    if e.kind == "run_summary" {
      continue;
    }
    *reasons.entry(e.reason.clone()).or_default() += 1;
    if let Some(h) = &e.harness {
      let route = format!("{h} · {}", e.model.as_deref().unwrap_or("(harness default)"));
      let entry = routes.entry(route).or_default();
      if e.kind == "retry" {
        entry.1 += 1;
      } else {
        entry.0 += 1;
        *entry.2.entry(e.reason.clone()).or_default() += 1;
      }
    }
  }
  if routes.is_empty() {
    println!("no route-attributed failures recorded yet (routes appear on failed skill events)");
  } else {
    println!("failures by route (harness · model):");
    for (route, (fails, retries, by_reason)) in &routes {
      let mut reason_bits: Vec<String> = by_reason.iter().map(|(r, n)| format!("{r} ×{n}")).collect();
      reason_bits.sort();
      let retry_note = if *retries > 0 { format!(", {retries} retried") } else { String::new() };
      println!("  {route}: {fails} failure(s){retry_note} — {}", reason_bits.join(", "));
    }
  }
  println!();
  println!("failures by reason (all events):");
  for (reason, n) in &reasons {
    println!("  {reason}: {n}");
  }
}

/// `scsh stats`: aggregate the durable run statistics — how long skills take per
/// harness·model route, against the workload they processed (commits + LOC over main).
fn stats_cmd(opts: &FailuresOpts, profile: Option<&str>) -> i32 {
  let records = stats::read_records();
  let matches_common = |r: &stats::StatRecord| {
    if let Some(s) = &opts.session {
      if r.session != *s {
        return false;
      }
    }
    if let Some(p) = profile {
      if r.profile.as_deref() != Some(p) {
        return false;
      }
    }
    true
  };
  let skill_rows: Vec<&stats::StatRecord> = records
    .iter()
    .filter(|r| r.kind == "skill" && matches_common(r))
    .filter(|r| {
      opts.skill.as_deref().is_none_or(|s| r.skill.as_deref() == Some(s) || r.skill_source.as_deref() == Some(s))
    })
    .filter(|r| opts.harness.as_deref().is_none_or(|h| r.harness.as_deref() == Some(h)))
    .filter(|r| opts.model.as_deref().is_none_or(|m| r.model.as_deref() == Some(m)))
    .collect();
  let run_rows: Vec<&stats::StatRecord> = records.iter().filter(|r| r.kind == "run" && matches_common(r)).collect();
  if skill_rows.is_empty() && run_rows.is_empty() {
    println!("no recorded runs match");
    hint(&format!("stats file: {}", stats::stats_path().display()));
    return 0;
  }
  if opts.raw {
    print_stats_raw(&skill_rows, &run_rows, opts.last);
    return 0;
  }
  print_run_aggregates(&run_rows);
  print_skill_aggregates(&skill_rows);
  hint(&format!("stats file: {} (individual rows: scsh stats --raw)", stats::stats_path().display()));
  0
}

fn print_stats_raw(skill_rows: &[&stats::StatRecord], run_rows: &[&stats::StatRecord], last: Option<usize>) {
  let mut rows: Vec<&stats::StatRecord> = skill_rows.iter().chain(run_rows.iter()).copied().collect();
  rows.sort_by_key(|r| r.ts);
  let keep = match last {
    Some(0) => rows.len(),
    Some(n) => n,
    None => 50,
  };
  let start = rows.len().saturating_sub(keep);
  if start > 0 {
    println!("… {start} earlier row(s) hidden — rerun with --last 0 for all");
  }
  for r in &rows[start..] {
    let when = runtime::format_utc_timestamp(r.ts);
    if r.kind == "run" {
      println!(
        "{when}  run    {:>7.1}s  profile={} session={} skills={}/{} ok  commits={} loc={}",
        r.duration_secs,
        r.profile.as_deref().unwrap_or("(default)"),
        r.session,
        r.skills_total.unwrap_or(0) - r.skills_failed.unwrap_or(0),
        r.skills_total.unwrap_or(0),
        r.commits,
        r.loc_total(),
      );
    } else {
      let route = r.route_label();
      let outcome = r.outcome.as_deref().unwrap_or("?");
      let retry = if r.attempts > 1 { " (retried)" } else { "" };
      println!(
        "{when}  skill  {:>7.1}s  {}  {route}  {outcome}{retry}  commits={} loc={}",
        r.duration_secs,
        r.skill_source.as_deref().or(r.skill.as_deref()).unwrap_or("?"),
        r.commits,
        r.loc_total(),
      );
    }
  }
}

fn print_run_aggregates(run_rows: &[&stats::StatRecord]) {
  if run_rows.is_empty() {
    return;
  }
  use std::collections::BTreeMap;
  let mut by_profile: BTreeMap<String, Vec<&stats::StatRecord>> = BTreeMap::new();
  for r in run_rows {
    by_profile.entry(r.profile.clone().unwrap_or_else(|| "(default)".into())).or_default().push(r);
  }
  println!("runs by profile:");
  for (profile, rows) in &by_profile {
    let n = rows.len() as f64;
    let mean_secs: f64 = rows.iter().map(|r| r.duration_secs).sum::<f64>() / n;
    let mean_commits: f64 = rows.iter().map(|r| r.commits as f64).sum::<f64>() / n;
    let mean_loc: f64 = rows.iter().map(|r| r.loc_total() as f64).sum::<f64>() / n;
    let failed_runs = rows.iter().filter(|r| r.skills_failed.unwrap_or(0) > 0).count();
    println!(
      "  {profile}: {} run(s), avg {:.0}s, avg workload {:.1} commits / {:.0} LOC{}",
      rows.len(),
      mean_secs,
      mean_commits,
      mean_loc,
      if failed_runs > 0 { format!(", {failed_runs} with failures") } else { String::new() },
    );
  }
  println!();
}

fn print_skill_aggregates(skill_rows: &[&stats::StatRecord]) {
  if skill_rows.is_empty() {
    return;
  }
  use std::collections::BTreeMap;
  // Group by (skill_source, harness · model (effort)) — "each reviewer, each route".
  let mut groups: BTreeMap<(String, String), Vec<&stats::StatRecord>> = BTreeMap::new();
  for r in skill_rows {
    let skill = r.skill_source.clone().or_else(|| r.skill.clone()).unwrap_or_else(|| "?".into());
    groups.entry((skill, r.route_label())).or_default().push(r);
  }
  let skill_w = groups.keys().map(|(s, _)| s.len()).max().unwrap_or(5).max(5);
  let route_w = groups.keys().map(|(_, r)| r.len()).max().unwrap_or(5).max(5);
  println!("skills by route (durations exclude cache hits):");
  println!(
    "  {:<skill_w$}  {:<route_w$}  {:>4} {:>3} {:>4} {:>5} {:>5}  {:>7} {:>7} {:>7}  {:>8} {:>7}",
    "skill", "route", "runs", "ok", "fail", "cache", "retry", "avg s", "min s", "max s", "~commits", "~LOC"
  );
  for ((skill, route), rows) in &groups {
    let agg = stats::aggregate_skills(rows);
    println!(
      "  {:<skill_w$}  {:<route_w$}  {:>4} {:>3} {:>4} {:>5} {:>5}  {:>7.1} {:>7.1} {:>7.1}  {:>8.1} {:>7.0}",
      skill,
      route,
      agg.runs,
      agg.ok,
      agg.failed,
      agg.cached,
      agg.retried,
      agg.mean_secs,
      agg.min_secs,
      agg.max_secs,
      agg.mean_commits,
      agg.mean_loc,
    );
  }
}

/// `scsh prune`: show the daemon's run-dir cleanup queue; `--now` forces a janitor pass
/// (through the daemon when it is running, else directly on the persisted queue).
fn prune_cmd(now_flag: bool) -> i32 {
  let port = daemon::daemon_port();
  let queue = daemon::prune::PruneQueue::load(port);
  if !now_flag {
    if queue.jobs.is_empty() {
      ok("run-dir prune queue is empty");
      return 0;
    }
    let now = daemon::now_unix_secs();
    println!("{} pending run-dir prune job(s):", queue.jobs.len());
    for j in &queue.jobs {
      let outcome = if j.outcome_ok { "ok" } else { "failed" };
      let when =
        if now >= j.eligible_at { "eligible now".to_string() } else { format!("eligible in {}s", j.eligible_at - now) };
      println!("  {}  ({outcome} run, {when})", j.run_dir);
    }
    hint("delete every eligible dir now with: scsh prune --now");
    return 0;
  }
  let before = queue.jobs.len();
  if daemon::daemon_port_reachable(port) {
    if !daemon::post_once(port, "/api/v1/prune/tick", "{}") {
      fail("session browser daemon is running but rejected the prune request");
      return 1;
    }
  } else {
    // No daemon: run one pass directly on the persisted queue.
    let mut q = queue;
    let _ = q.tick(daemon::now_unix_secs());
    q.save(port);
  }
  let after = daemon::prune::PruneQueue::load(port).jobs.len();
  ok(&format!("prune pass complete: {before} job(s) before, {after} remaining"));
  0
}

/// Absolute repo path for the session browser (canonical when possible).
fn repo_path_for_session(root: &Path) -> String {
  daemon::absolutize_repo_path(root)
}

/// Best-effort daemon teardown on every exit path (build failure, skill failure, panic, early return).
struct DaemonSession {
  client: Option<std::sync::Arc<daemon::Client>>,
  ping_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
  registered: bool,
}

impl DaemonSession {
  fn cleanup(&mut self) {
    if let Some(flag) = self.ping_active.take() {
      flag.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(c) = self.client.take() {
      if self.registered {
        c.finish_session();
        ok(&format!("session {}", c.session_url()));
      } else {
        c.flush();
      }
    }
  }
}

impl Drop for DaemonSession {
  fn drop(&mut self) {
    self.cleanup();
  }
}

fn build_and_run(rt: &Runtime, root: &std::path::Path, skills: &[&ResolvedInvocation], profile: Option<&str>) -> i32 {
  ui::signals::install();

  // Session browser daemon — `scsh run` always tries to attach; ephemeral auto-start when needed.
  let session_id = daemon::new_session_id();
  let mut daemon_session = DaemonSession { client: None, ping_active: None, registered: false };
  match daemon::ensure_for_run() {
    Ok(()) => {
      let client = std::sync::Arc::new(daemon::Client::new(session_id.clone()));
      let skill_meta: Vec<(&str, &str)> = skills.iter().map(|s| (s.name.as_str(), s.harness.as_str())).collect();
      if client.register_session(&repo_path_for_session(root), &current_branch(root), profile, &skill_meta) {
        ok(&format!("track progress at {}", client.session_url()));
        let ping_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ping_flag = std::sync::Arc::clone(&ping_active);
        let ping_client = std::sync::Arc::clone(&client);
        std::thread::spawn(move || {
          while ping_flag.load(std::sync::atomic::Ordering::Relaxed) {
            ping_client.ping();
            std::thread::sleep(Duration::from_secs(2));
          }
        });
        daemon_session.client = Some(client);
        daemon_session.ping_active = Some(ping_active);
        daemon_session.registered = true;
      } else {
        hint(&format!("session browser daemon is up but registration failed; try {}", client.session_url()));
      }
    }
    Err(e) => {
      hint(&format!("session browser daemon unavailable ({e}); continuing without live browser UI"));
    }
  }
  let daemon_client = daemon_session.client.clone();

  let (uid, gid) = host_ids();
  let secs = now_secs();
  if !keep_run_dirs() {
    let swept = sweep_stale_run_dirs(secs);
    if swept > 0 {
      hint(&format!("swept {swept} stale run dir{} from /tmp", plural(swept)));
    }
  }
  let base = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());

  let needs_opencode = skills.iter().any(|s| s.harness == config::Harness::Opencode);
  let needs_claude = skills.iter().any(|s| s.harness == config::Harness::Claude);
  let needs_codex = skills.iter().any(|s| s.harness == config::Harness::Codex);
  if needs_opencode && opencode_auth_enabled() && runtime::opencode_auth_ready() {
    ok("opencode creds found (auth.json and opencode config bind-mounted when present)");
  }
  if needs_claude && runtime::claude_container_auth_ready() {
    let via =
      if runtime::claude_oauth_token().is_some() { "CLAUDE_CODE_OAUTH_TOKEN" } else { "~/.claude/.credentials.json" };
    ok(&format!("claude credentials found ({via} forwarded into claude skills)"));
  }
  if needs_codex && codex_auth_enabled() && runtime::codex_container_auth_ready() {
    let via = if runtime::codex_auth_file_on_host().is_some() { "~/.codex/auth.json" } else { "OPENAI_API_KEY" };
    ok(&format!("codex credentials found ({via} forwarded into codex skills)"));
  }
  let needs_grok = skills.iter().any(|s| s.harness == config::Harness::Grok);
  if needs_grok && grok_auth_enabled() && runtime::grok_container_auth_ready() {
    let via = if runtime::grok_auth_file_on_host().is_some() { "~/.grok/auth.json" } else { "XAI_API_KEY" };
    ok(&format!("grok credentials found ({via} forwarded into grok skills)"));
  }
  let needs_cursor = skills.iter().any(|s| s.harness == config::Harness::Cursor);
  if needs_cursor && cursor_auth_enabled() && runtime::cursor_container_auth_ready() {
    let via = if runtime::cursor_api_key().is_some() {
      "CURSOR_API_KEY"
    } else if runtime::cursor_auth_file_on_host().is_some() {
      "auth.json"
    } else {
      "macOS keychain"
    };
    ok(&format!("cursor credentials found ({via} forwarded into cursor skills)"));
  }

  let ui = ui::screen::LiveUi::new(console::user_attended_stderr(), daemon_client.clone());

  let df = runtime::dockerfile();
  let tz = runtime::host_timezone();
  // Harness build order: first time each harness appears in the manifest (not enum sort).
  let mut harness_list = Vec::new();
  let mut seen_harness = std::collections::BTreeSet::new();
  for s in skills {
    if seen_harness.insert(s.harness) {
      harness_list.push(s.harness);
    }
  }
  let rt_name = rt.name.clone();
  let base_fp = runtime::base_image_fingerprint(&df, uid, gid, &tz);
  let base_needs_build = !runtime::image_is_up_to_date(&rt_name, runtime::BASE_IMAGE_TAG, &base_fp);
  let mut harness_builds: Vec<runtime::ImageBuildSpec> = Vec::new();
  for &h in &harness_list {
    let spec = runtime::image_build_spec(h, &df, uid, gid, &tz);
    if !runtime::image_is_up_to_date(&rt_name, &spec.tag, &spec.fingerprint) {
      harness_builds.push(spec);
    }
  }
  let any_image_build = base_needs_build || !harness_builds.is_empty();

  // Base image first, then one build proc per harness that actually needs rebuilding;
  // the harness images only depend on the base, so they build in parallel.
  let mut base_build = None;
  if base_needs_build {
    let base_label = format!("using {} · build base", backend_name(&rt.name));
    let p = ui.proc(base_label.clone(), true);
    if let Some(c) = &daemon_client {
      c.proc_add(p.index(), &base_label, daemon::ProcKind::Build, None, None, None);
    }
    base_build = Some(p);
  }
  let mut harness_build_procs: Vec<ui::screen::Proc> = Vec::with_capacity(harness_builds.len());
  for spec in &harness_builds {
    let label = format!("using {} · build {}", backend_name(&rt.name), spec.harness.as_str());
    let p = ui.proc(label.clone(), true);
    if let Some(c) = &daemon_client {
      c.proc_add(p.index(), &label, daemon::ProcKind::Build, None, Some(spec.harness.as_str()), None);
    }
    harness_build_procs.push(p);
  }
  let mut skill_procs = Vec::with_capacity(skills.len());
  for skill in skills {
    let label = format!("{}: {}", skill.harness.as_str(), skill.name);
    let p = ui.proc(label.clone(), false);
    if let Some(c) = &daemon_client {
      c.proc_add(
        p.index(),
        &label,
        daemon::ProcKind::Skill,
        Some(skill.name.as_str()),
        Some(skill.harness.as_str()),
        skill.model.as_deref(),
      );
    }
    if any_image_build {
      p.note("waiting for image build…");
    }
    skill_procs.push(p);
  }
  ui.pin_board_to_top();

  let mut build_failed = if let Some(ref base) = base_build {
    base.start();
    match run_build(base, &rt_name, runtime::BASE_IMAGE_TAG, runtime::BASE_IMAGE_TARGET, &df, uid, gid, &base_fp) {
      Ok(()) => {
        base.finish_ok(None);
        None
      }
      Err(e) => {
        base.finish_fail(failure::reason::BUILD_FAILED, Some(&e.0));
        Some(e)
      }
    }
  } else {
    None
  };

  if build_failed.is_none() {
    // Harness images depend only on the freshly built base, so they build in parallel
    // (one thread per image, same scoped-thread idiom as the skill runs below). All
    // builds run to completion; the first failure is the one reported.
    build_failed = std::thread::scope(|scope| {
      let handles: Vec<_> = harness_build_procs
        .iter()
        .zip(harness_builds.iter())
        .map(|(build, spec)| {
          let rt_name = &rt_name;
          let df = &df;
          scope.spawn(move || {
            build.start();
            match run_build(build, rt_name, &spec.tag, &spec.target, df, uid, gid, &spec.fingerprint) {
              Ok(()) => {
                build.finish_ok(None);
                None
              }
              Err(e) => {
                build.finish_fail(failure::reason::BUILD_FAILED, Some(&e.0));
                Some(e)
              }
            }
          })
        })
        .collect();
      handles
        .into_iter()
        .filter_map(|h| h.join().unwrap_or_else(|_| Some(("image build thread panicked".to_string(), 1))))
        .next()
    });
  }
  if let Some((msg, code)) = build_failed {
    ui.finish();
    fail(&msg);
    return code;
  }

  let base_ref = base.as_deref();
  for p in &skill_procs {
    p.note("starting…");
  }
  let run_started = std::time::Instant::now();
  let workload = stats::workload_of_repo(root);
  let outcomes: Vec<SkillRun> = std::thread::scope(|scope| {
    let dc = daemon_client.clone();
    let ui_ref = &ui;
    let session_ref = session_id.as_str();
    let handles: Vec<_> = skills
      .iter()
      .zip(skill_procs)
      .map(|(&skill, p)| {
        let dc = dc.clone();
        scope.spawn(move || {
          let attempt_started = std::time::Instant::now();
          let mut first = run_one_skill(skill, rt, root, secs, p, base_ref, dc.clone());
          first.duration_secs = attempt_started.elapsed().as_secs_f64();
          if first.ok {
            return first;
          }
          // One automatic retry for transient infrastructure failures (fresh clone, fresh
          // container, new live-board row). Deterministic failures return as-is.
          let transient = first.fail_reason.as_deref().is_some_and(failure::is_transient);
          if !transient || !failure::retry_enabled() {
            return first;
          }
          let reason = first.fail_reason.as_deref().unwrap_or("unknown");
          failure::log_retry(session_ref, &skill.name, skill.harness.as_str(), skill.model.as_deref(), reason);
          let label = format!("{}: {} (retry)", skill.harness.as_str(), skill.name);
          let p2 = ui_ref.proc(label.clone(), false);
          if let Some(c) = &dc {
            c.proc_add(
              p2.index(),
              &label,
              daemon::ProcKind::Skill,
              Some(skill.name.as_str()),
              Some(skill.harness.as_str()),
              skill.model.as_deref(),
            );
          }
          let retry_started = std::time::Instant::now();
          let mut second = run_one_skill(skill, rt, root, secs, p2, base_ref, dc);
          second.duration_secs = retry_started.elapsed().as_secs_f64();
          second.attempts = 2;
          second
        })
      })
      .collect();
    handles
      .into_iter()
      .zip(skills)
      .map(|(h, skill)| {
        h.join().unwrap_or_else(|_| {
          failure::log_skill(
            failure::reason::THREAD_PANICKED,
            &skill.name,
            "skill thread panicked before reporting outcome",
          );
          SkillRun::failed(failure::reason::THREAD_PANICKED, None, None, None)
        })
      })
      .collect()
  });

  // The run is over: restore the terminal and print the persistent ✓/✗ summary (attended; off a
  // TTY the per-proc lines already streamed). Everything below prints to the normal screen.
  ui.finish();

  // 3. The summary above carries each skill's ✓/✗ and detail; add run-dir/log pointers for any
  //    that failed, then the overall verdict.
  let n = outcomes.len();
  let failed = outcomes.iter().filter(|o| !o.ok).count();
  for (skill, o) in skills.iter().zip(outcomes.iter()).filter(|(_, o)| !o.ok) {
    if let Some(dir) = &o.run_dir {
      hint(&format!("run dir kept: {dir}"));
    }
    if let Some(log) = &o.log {
      hint(&format!("output log: {log}"));
    }
    let reason = o.fail_reason.as_deref().unwrap_or("unknown");
    let mut detail = String::new();
    if let Some(d) = &o.run_dir {
      detail.push_str(&format!("run dir: {d}\n"));
    }
    if let Some(l) = &o.log {
      detail.push_str(&format!("output log: {l}"));
    }
    failure::log_failed_skill(
      &session_id,
      &skill.name,
      skill.harness.as_str(),
      skill.model.as_deref(),
      reason,
      detail.trim(),
    );
  }
  if failed > 0 {
    failure::log_run_summary(&session_id, profile, failed, n);
    hint(&format!("failure log: {} (browse with `scsh failures`)", failure::log_path().display()));
  }

  // Persist run statistics (durable, ~/.scsh/stats.jsonl — browse with `scsh stats`): one
  // row per skill invocation with its route, outcome, duration, and the repo workload
  // (commits + LOC over main), plus one rollup row for the whole run.
  {
    let branch = current_branch(root);
    let repo = repo_path_for_session(root);
    for (skill, o) in skills.iter().zip(outcomes.iter()) {
      let outcome = if o.cached {
        "cached"
      } else if o.ok {
        "ok"
      } else {
        "fail"
      };
      stats::record(&stats::StatRecord {
        ts: secs,
        kind: "skill".into(),
        session: session_id.clone(),
        repo: repo.clone(),
        branch: branch.clone(),
        profile: profile.map(str::to_string),
        skill: Some(skill.name.clone()),
        skill_source: Some(skill.skill_source.clone()),
        harness: Some(skill.harness.as_str().to_string()),
        model: skill.model.clone(),
        effort: skill.effort.clone(),
        outcome: Some(outcome.into()),
        fail_reason: o.fail_reason.clone(),
        attempts: o.attempts,
        duration_secs: o.duration_secs,
        commits: workload.commits,
        loc_added: workload.loc_added,
        loc_deleted: workload.loc_deleted,
        skills_total: None,
        skills_failed: None,
      });
    }
    stats::record(&stats::StatRecord {
      ts: secs,
      kind: "run".into(),
      session: session_id.clone(),
      repo,
      branch,
      profile: profile.map(str::to_string),
      attempts: 1,
      duration_secs: run_started.elapsed().as_secs_f64(),
      commits: workload.commits,
      loc_added: workload.loc_added,
      loc_deleted: workload.loc_deleted,
      skills_total: Some(n as u64),
      skills_failed: Some(failed as u64),
      ..Default::default()
    });
  }

  // 4. Pull commits OUT from commit-enabled skills (host-only, after containers exit).
  //    Runs SEQUENTIALLY: each skill's new commits in its run clone (base..clone-HEAD)
  //    are fetched from the LOCAL clone path — not from GitHub — and cherry-picked onto
  //    the caller's branch. Only when commits: true AND the skill actually committed.
  //    Commits that don't apply cleanly are saved to scsh/incoming/<skill>-… instead.
  if let Some(base) = &base {
    let stamp = runtime::format_utc_timestamp(secs);
    for (skill, o) in skills.iter().zip(outcomes.iter()) {
      if !skill.commits {
        continue;
      }
      // A live clone integrates its commits directly; a commit-enabled cache HIT replays
      // the commits journaled in the cache, so a hit reproduces the commit, not just the result.
      let integration = if let Some(clone) = &o.clone_dir {
        integrate_commits(root, clone, base, &skill.name, &stamp)
      } else if let Some(patch) = &o.cached_commits {
        apply_cached_commits(root, patch, &skill.name, &stamp)
      } else {
        continue;
      };
      match integration {
        Ok(None) => {}
        Ok(Some(Integration::Applied { count })) => ok(&format!(
          "{}: brought in {count} commit{} (rebased onto {})",
          skill.name,
          plural(count),
          current_branch(root)
        )),
        Ok(Some(Integration::Saved { branch, count })) => warn(&format!(
          "{}: {count} commit{} didn't rebase cleanly — saved to branch {branch} (inspect, then merge/cherry-pick)",
          skill.name,
          plural(count)
        )),
        Err(e) => warn(&format!("{}: could not bring in commits — {e}", skill.name)),
      }
    }
  }

  // 5. Tidy up. A successful skill's clone has served its purpose — the result was
  //    collected and any commits integrated — so remove it (the container was already
  //    `--rm`; this is the host-side scratch). A FAILED skill's clone is kept for
  //    inspection (its path was printed above). Opt out entirely with SCSH_KEEP_RUNS=1.
  if !keep_run_dirs() {
    for o in outcomes.iter().filter(|o| o.ok) {
      if let Some(clone) = &o.clone_dir {
        let _ = std::fs::remove_dir_all(clone);
      }
    }
  }

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
/// residue the orchestrator still needs afterward — run-dir/log pointers and commit replay.
struct SkillRun {
  ok: bool,
  /// Stable reason code when `ok == false`.
  fail_reason: Option<String>,
  /// Served from the content-addressed cache (no clone, no container).
  cached: bool,
  /// Wall-clock seconds of the (final) attempt — set by the orchestrator, for stats.
  duration_secs: f64,
  /// 1, or 2 when the transient-failure retry ran — set by the orchestrator.
  attempts: u64,
  /// The `/tmp` run dir, kept for inspection when the skill failed.
  run_dir: Option<String>,
  /// Host path to the skill's output log, when its container actually ran.
  log: Option<String>,
  /// The skill's clone, set whenever the clone succeeded (whatever the outcome), so
  /// a commit-enabled skill's commits can be brought back afterward. `None` if no
  /// clone was made (e.g. a refused or pre-clone failure).
  clone_dir: Option<PathBuf>,
  /// For a commit-enabled skill served from cache: the journaled commits as a git
  /// `format-patch` mbox, replayed onto the caller's branch so a hit reproduces the
  /// commit side effect (not just the result file). `None` otherwise.
  cached_commits: Option<String>,
}

impl SkillRun {
  fn base() -> SkillRun {
    SkillRun {
      ok: false,
      fail_reason: None,
      cached: false,
      duration_secs: 0.0,
      attempts: 1,
      run_dir: None,
      log: None,
      clone_dir: None,
      cached_commits: None,
    }
  }
  fn ok(log: String, clone_dir: Option<PathBuf>) -> SkillRun {
    SkillRun { ok: true, log: Some(log), clone_dir, ..SkillRun::base() }
  }
  fn failed(reason: &str, run_dir: Option<String>, log: Option<String>, clone_dir: Option<PathBuf>) -> SkillRun {
    SkillRun { fail_reason: Some(reason.into()), run_dir, log, clone_dir, ..SkillRun::base() }
  }
  /// A cache hit: the result was restored from the cache without running the skill (no
  /// clone, no container). `cached_commits` carries any journaled commits to replay, so a
  /// hit for a commit-enabled skill still reproduces the commit.
  fn cached(cached_commits: Option<String>) -> SkillRun {
    SkillRun { ok: true, cached: true, cached_commits, ..SkillRun::base() }
  }
}

/// One detail string for a failed skill — shown in the terminal summary and session browser.
fn skill_fail_detail(why: &str, harness: config::Harness, run_dir: Option<&str>, log: Option<&str>) -> String {
  let mut parts = vec![why.to_string()];
  if let Some(d) = run_dir {
    parts.push(format!("run dir: {d}"));
  }
  if let Some(l) = log {
    parts.push(format!("output log: {l}"));
    if runtime::harness_verbose_enabled() {
      match harness {
        config::Harness::Claude => parts.push(format!("claude debug log: {l}.debug")),
        config::Harness::Codex => parts.push(format!("codex final message: {l}.last")),
        config::Harness::Grok => parts.push(format!("grok debug log: {l}.debug")),
        config::Harness::Cursor => {}
        config::Harness::Opencode => {}
      }
    }
  }
  parts.join("\n")
}

/// Run a single skill end to end in its own clone and container, driving `spinner`
/// through its phases and finishing it ✓/✗. Returns the structured outcome.
fn run_one_skill(
  skill: &ResolvedInvocation, rt: &Runtime, root: &Path, secs: u64, spinner: ui::screen::Proc, base: Option<&str>,
  daemon_client: Option<std::sync::Arc<daemon::Client>>,
) -> SkillRun {
  // Mark the row running so its clock starts and output stamps are relative to here.
  spinner.start();
  // Resolve forwarded env first: a missing required (${VAR:?…}) variable refuses
  // the skill before any work — no clone, no container.
  let env = match resolve_env(&skill.env) {
    Ok(mut e) => {
      e.push(("SCSH_RESULT".to_string(), skill.result.clone()));
      e
    }
    Err(message) => {
      spinner.finish_fail(failure::reason::ENV_UNRESOLVED, Some(&message));
      return SkillRun::failed(failure::reason::ENV_UNRESOLVED, None, None, None);
    }
  };

  // Content-addressed cache: if this exact repo content + skill + env was run before,
  // restore the cached result and finish — no clone, no container, no commit. (The key
  // is computed from the caller's committed state, which is what the clone would be.)
  let key = cache_key(root, skill, &env);
  if let Some(key) = &key {
    if let Some(entry) = cache_lookup(root, key) {
      if restore_cached_result(root, &skill.result, &entry.result).is_ok() {
        let line = match json::message(&entry.result) {
          Some(m) => format!("{}  (cached)", first_line(&m)),
          None => "(cached)".to_string(),
        };
        spinner.finish_ok(Some(&line));
        // Carry any journaled commits so they're replayed onto the caller's branch — a hit
        // for a commit-enabled skill reproduces the commit, not just the result file.
        return SkillRun::cached(entry.commits);
      }
    }
  }

  // Own run dir on the HOST (push IN). Either a full clone bind-mounted into the container,
  // or (macOS Apple Container) a bare transport repo + git daemon the container clones from.
  // After the container exits, scsh pulls the result file OUT; commits too when commits: true.
  spinner.note("preparing repo…");
  let run_dir = match prepare_run_dir(secs, &skill.name, &rt.name) {
    Ok(d) => d,
    Err(e) => {
      spinner.finish_fail(failure::reason::RUN_DIR, Some(&e));
      return SkillRun::failed(failure::reason::RUN_DIR, None, None, None);
    }
  };
  let run_dir_str = run_dir.to_string_lossy().into_owned();
  let git_transport = runtime::uses_git_transport(&rt.name);
  let mut git_daemon = None;
  if git_transport {
    if let Err(e) = prepare_git_transport(root, &run_dir, skill.commits, &spinner) {
      spinner.finish_fail(failure::reason::GIT_TRANSPORT, Some(&e));
      return SkillRun::failed(failure::reason::GIT_TRANSPORT, Some(run_dir_str), None, None);
    }
    match GitTransport::start(&run_dir) {
      Ok(d) => git_daemon = Some(d),
      Err(e) => {
        spinner.finish_fail(failure::reason::GIT_DAEMON, Some(&e));
        return SkillRun::failed(failure::reason::GIT_DAEMON, Some(run_dir_str), None, None);
      }
    }
  } else if let Err(e) = clone_into(root, &run_dir, &spinner) {
    spinner.finish_fail(failure::reason::CLONE, Some(&e));
    return SkillRun::failed(failure::reason::CLONE, Some(run_dir_str), None, None);
  }
  // From here the clone exists — carry it so a commit-enabled skill's commits can be
  // brought back even if a later step fails.
  let clone_dir = Some(run_dir.clone());

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
  // Copy host opencode auth/config into the run clone and bind-mount from there.
  let opencode_forward = if skill.harness == config::Harness::Opencode && opencode_auth_enabled() {
    forward_opencode(&run_dir)
  } else {
    None
  };
  if opencode_forward.is_some() {
    prepare_opencode_mount_dirs(&run_dir);
  }
  let claude_auth = if skill.harness == config::Harness::Claude && claude_auth_enabled() {
    forward_claude_auth(&run_dir)
  } else {
    None
  };
  let codex_auth =
    if skill.harness == config::Harness::Codex && codex_auth_enabled() { forward_codex(&run_dir) } else { None };
  let grok_auth =
    if skill.harness == config::Harness::Grok && grok_auth_enabled() { forward_grok(&run_dir) } else { None };
  let cursor_auth =
    if skill.harness == config::Harness::Cursor && cursor_auth_enabled() { forward_cursor(&run_dir) } else { false };
  let tag = runtime::image_tag(skill.harness);
  // Claude needs no extra mounts: its forwarded config lives under the run clone's
  // tmp/.claude-auth (the image's CLAUDE_CONFIG_DIR), riding along with the repo mount —
  // and stays WRITABLE, which the interactive TUI requires (single-file bind mounts are
  // read-only under Apple containers, and an unwritable config re-triggers onboarding).
  let vols: Vec<(String, String)> = if let Some(ref forward) = opencode_forward {
    runtime::opencode_forward_mounts(forward)
  } else {
    runtime::harness_volumes(skill.harness)
  };
  let vol_refs: Vec<(&str, &str)> = vols.iter().map(|(h, m)| (h.as_str(), m.as_str())).collect();
  let mut container_env = env.clone();
  if skill.harness == config::Harness::Claude {
    if let Some(token) = runtime::claude_oauth_token() {
      container_env.push((runtime::CLAUDE_OAUTH_TOKEN_ENV.to_string(), token));
    }
  }
  if skill.harness == config::Harness::Codex {
    if let Ok(key) = std::env::var(runtime::OPENAI_API_KEY_ENV) {
      if !key.is_empty() {
        container_env.push((runtime::OPENAI_API_KEY_ENV.to_string(), key));
      }
    }
  }
  if skill.harness == config::Harness::Grok {
    if let Ok(key) = std::env::var(runtime::XAI_API_KEY_ENV) {
      if !key.is_empty() {
        container_env.push((runtime::XAI_API_KEY_ENV.to_string(), key));
      }
    }
  }
  if skill.harness == config::Harness::Cursor {
    if let Some(key) = runtime::cursor_api_key() {
      container_env.push((runtime::CURSOR_API_KEY_ENV.to_string(), key));
    }
  }
  container_env.extend(runtime::harness_container_env(skill.harness));
  if let Some(d) = &git_daemon {
    container_env.extend(d.env());
  }
  let harness = runtime::harness_command(
    skill.harness,
    skill.model.as_deref(),
    skill.effort.as_deref(),
    &skill.skill_source,
    &skill.result,
    skill.terminal,
  );
  let cmd = if git_transport {
    runtime::git_transport_entry(&harness, skill.commits, SCSH_COMMIT_NAME, SCSH_COMMIT_EMAIL)
  } else {
    harness
  };
  let repo_mount = if git_transport { runtime::RepoMountMode::TmpOnly } else { runtime::RepoMountMode::Full };
  let run = runtime::run_command(&rt.name, &tag, &run_dir_str, &name, &container_env, &vol_refs, &cmd, repo_mount);
  let timeout = skill.timeout.map(Duration::from_secs);
  let _container = ui::signals::ContainerGuard::new(&rt.name, &name);
  if let Some(c) = &daemon_client {
    c.container_event(spinner.index(), "start", &name);
    // The bind-mounted cast grows on the host while the harness runs; registering it now
    // lets the session browser download/replay the recording mid-run.
    c.proc_cast(spinner.index(), &run_dir.join(runtime::RUN_CAST_REL).to_string_lossy());
  }
  let result = spinner.run_timed(&run[0], &run[1..], timeout);
  if let Some(c) = &daemon_client {
    c.container_event(spinner.index(), "stop", &name);
  }
  // Run dirs are pruned shortly after the skill ends (on any outcome); keep the recording
  // under the caller repo's gitignored tmp/casts/ so it can be revisited later.
  if let Some(durable) = persist_cast(root, &run_dir, &skill.name, secs) {
    if let Some(c) = &daemon_client {
      c.proc_cast(spinner.index(), &durable);
    }
  }
  if let Some(p) = &claude_auth {
    let _ = std::fs::remove_dir_all(p);
  }
  if let Some(p) = &opencode_forward {
    let _ = std::fs::remove_dir_all(p);
  }
  if let Some(p) = &codex_auth {
    scrub_codex_credentials(p);
  }
  if let Some(p) = &grok_auth {
    scrub_grok_credentials(p);
  }
  if cursor_auth {
    scrub_cursor_credentials(&run_dir);
  }
  match result {
    Ok((true, _, _)) => {}
    Ok((false, true, _)) => {
      // Timed out: the client was killed; stop the container too (best effort).
      ui::signals::stop_container(&rt.name, &name);
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let why = format!("timed out after {}s", skill.timeout.unwrap_or(0));
      let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::CONTAINER_TIMEOUT, Some(&detail));
      return SkillRun::failed(failure::reason::CONTAINER_TIMEOUT, Some(run_dir_str), Some(log), clone_dir);
    }
    Ok((false, false, last)) => {
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let tail = spinner.tail_lines(failure::FAILURE_TAIL_LINES);
      let why = failure::failure_excerpt(last.as_deref(), &tail, "harness exited non-zero (no output captured)");
      let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::HARNESS_NONZERO, Some(&detail));
      return SkillRun::failed(failure::reason::HARNESS_NONZERO, Some(run_dir_str), Some(log), clone_dir);
    }
    Err(e) => {
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let why = format!("could not run container: {e}");
      let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::CONTAINER_RUN, Some(&detail));
      return SkillRun::failed(failure::reason::CONTAINER_RUN, Some(run_dir_str), Some(log), clone_dir);
    }
  }

  // Pull the result file OUT of the run clone into the caller repo (host-side, always).
  // The result file is required: missing → this skill (and the whole run) fails.
  match collect_skill_result(root, &run_dir, &skill.result, secs) {
    Ok(dest) => {
      // Cache the result content under this run's key, so an identical future run
      // (same repo content + skill + env) is a hit. Then show the skill's *message*,
      // not just the file (its `result`/`message`/sole field — see json::message),
      // falling back to the result path; a multi-line message shows its first line.
      let content = std::fs::read_to_string(&dest).ok();
      if let (Some(key), Some(c)) = (&key, &content) {
        // Journal a commit-enabled skill's new commits (base..clone-HEAD) as a patch
        // alongside the result, so a future cache hit can replay them.
        let commits =
          if skill.commits { base.and_then(|b| commit_patch(&runtime::commits_fetch_path(&run_dir), b)) } else { None };
        cache_store(root, key, c, commits.as_deref());
      }
      let message = content.as_deref().and_then(json::message);
      let headline = message.as_deref().map(first_line).unwrap_or(skill.result.as_str());
      spinner.finish_ok(Some(headline));
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, true);
      SkillRun::ok(log, clone_dir)
    }
    Err(e) => {
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let detail = skill_fail_detail(&e, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::RESULT_MISSING, Some(&detail));
      SkillRun::failed(failure::reason::RESULT_MISSING, Some(run_dir_str), Some(log), clone_dir)
    }
  }
}

/// Copy a run's asciinema recording into the caller repo's gitignored `tmp/casts/`, named
/// `<skill>-<YYYYMMDD-HHMMSS>-utc.cast`, so recordings survive run-dir pruning and can be
/// revisited later (the session browser replays from this path too). Returns the
/// destination when the copy landed.
fn persist_cast(root: &Path, run_dir: &Path, skill_name: &str, epoch_secs: u64) -> Option<String> {
  let src = run_dir.join(runtime::RUN_CAST_REL);
  if !src.is_file() {
    return None;
  }
  let dir = root.join("tmp").join("casts");
  std::fs::create_dir_all(&dir).ok()?;
  // Full second-resolution timestamp PLUS a random 6-letter nonce: every run within one
  // `scsh run` shares `epoch_secs`, so the timestamp alone would overwrite prior casts.
  let dest = dir.join(format!(
    "{skill_name}-{}-utc-{}.cast",
    runtime::format_utc_timestamp(epoch_secs),
    runtime::random_nonce_6()
  ));
  std::fs::copy(&src, &dest).ok()?;
  Some(dest.to_string_lossy().into_owned())
}

/// Recreate a skill's result file from a cached `content` (creating parent dirs), so a
/// cache hit leaves the same result on disk a real run would have collected.
fn restore_cached_result(root: &Path, result_rel: &str, content: &str) -> std::io::Result<()> {
  let dest = root.join(result_rel);
  if let Some(parent) = dest.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(dest, content)
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

/// Age (seconds) past which a leftover `/tmp/scsh-*-run-*` clone is treated as stale and
/// swept at the next run's startup. A full day — comfortably longer than any skill run (skill
/// timeouts are in minutes) — so a concurrently-running scsh's fresh clone is never removed.
const STALE_RUN_DIR_SECS: u64 = 24 * 60 * 60;

/// Best-effort sweep of stale per-run clones left under `/tmp` by earlier runs — a failed
/// skill's kept clone, or a clone orphaned by a crash before cleanup. Only entries matching
/// [`runtime::is_scsh_run_dir_name`] AND older than [`STALE_RUN_DIR_SECS`] are removed,
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
    if !runtime::is_scsh_run_dir_name(&name) {
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

/// Create the per-run scratch dir under `/tmp`. Docker/podman use a UTC-stamped name with
/// `-2`, `-3`, … suffixes on collision; Apple `container` uses a random nonce and retries
/// with a fresh nonce when the dir already exists (container IDs must stay ≤ 64 chars).
fn prepare_run_dir(secs: u64, skill: &str, runtime: &str) -> Result<PathBuf, String> {
  if runtime == "container" {
    for _ in 1..=100 {
      let base = runtime::run_dir_name(secs, skill, runtime);
      let dir = PathBuf::from("/tmp").join(&base);
      match std::fs::create_dir(&dir) {
        Ok(()) => return Ok(dir),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
        Err(e) => return Err(format!("could not create run dir {}: {e}", dir.display())),
      }
    }
    return Err("could not create a unique run dir under /tmp".into());
  }
  let base = runtime::run_dir_name(secs, skill, runtime);
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
/// local one so the container sees them all. Used when bind-mounting the run dir
/// (Linux host → Linux container). Skills must not reach out to git remotes.
fn clone_into(root: &Path, run_dir: &Path, spinner: &ui::screen::Proc) -> Result<(), String> {
  spinner.note("cloning…");
  let cmd = runtime::clone_command(&root.to_string_lossy(), &run_dir.to_string_lossy());
  let (ok, last) = spinner.run(&cmd[0], &cmd[1..]).map_err(|e| format!("failed to run git clone: {e}"))?;
  if !ok {
    return Err(match last {
      Some(l) if !l.is_empty() => format!("git clone failed: {l}"),
      _ => "git clone failed".to_string(),
    });
  }
  materialize_branches(run_dir);
  set_clone_identity(run_dir);
  spinner.note("checking clone integrity…");
  let fsck = runtime::fsck_command(&run_dir.to_string_lossy());
  spinner.emit("git fsck --no-progress…");
  let fsck_started = Instant::now();
  let (ok, last) = spinner.run(&fsck[0], &fsck[1..]).map_err(|e| format!("failed to run git fsck: {e}"))?;
  let fsck_secs = fsck_started.elapsed().as_secs_f64();
  spinner.emit(&format!("git fsck {} ({})", if ok { "ok" } else { "failed" }, ui::clock::format_elapsed(fsck_secs),));
  if !ok {
    return Err(match last {
      Some(l) if !l.is_empty() => format!("git fsck failed on run clone: {l}"),
      _ => "git fsck failed on run clone".to_string(),
    });
  }
  Ok(())
}

/// macOS Apple Container push IN: host `git push` into a bare transport repo; the container
/// clones from a short-lived `git daemon` (Linux-owned `.git`). Only `run_dir/tmp` is mounted.
fn prepare_git_transport(root: &Path, run_dir: &Path, commits: bool, spinner: &ui::screen::Proc) -> Result<(), String> {
  std::fs::create_dir_all(run_dir.join("tmp"))
    .map_err(|e| format!("could not create {}: {e}", run_dir.join("tmp").display()))?;
  spinner.note("pushing…");
  let bare = run_dir.join(runtime::TRANSPORT_BARE);
  runtime::push_transport_refs(root, &bare).map_err(|e| {
    spinner.emit(&format!("git push failed: {e}"));
    e
  })?;
  if commits {
    runtime::init_bare_repo(&run_dir.join(runtime::PULL_BARE))?;
  }
  Ok(())
}

/// Per-run `git daemon` serving `transport.git` (and optionally `pull.git`) from a run dir.
struct GitTransport {
  child: std::process::Child,
  port: u16,
}

impl GitTransport {
  fn start(run_dir: &Path) -> Result<Self, String> {
    let port = runtime::pick_ephemeral_port()?;
    let base = run_dir.to_string_lossy();
    let child = Command::new("git")
      .args([
        "daemon",
        "--reuseaddr",
        &format!("--base-path={base}"),
        "--export-all",
        "--enable=receive-pack",
        &format!("--port={port}"),
        "--listen=0.0.0.0",
      ])
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .spawn()
      .map_err(|e| format!("could not start git daemon: {e}"))?;
    std::thread::sleep(Duration::from_millis(100));
    Ok(Self { child, port })
  }

  fn env(&self) -> Vec<(String, String)> {
    vec![(runtime::GIT_TRANSPORT_PORT_ENV.to_string(), self.port.to_string())]
  }
}

impl Drop for GitTransport {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

/// The deliberately unmistakable identity scsh stamps on commits a skill makes in its
/// clone — a "neon cyberpunk" bot that is never a real contributor. These commits are
/// LOCAL-ONLY by design (scsh rebases them onto your branch, it never pushes), so if this
/// author ever shows up in a code review or a pushed commit list, you pushed something
/// you shouldn't have. See `scsh help cache`.
const SCSH_COMMIT_NAME: &str = "dkorolev-neon-elon-bot";
const SCSH_COMMIT_EMAIL: &str = "dmitry.korolev+elon-presley@gmail.com";

/// Give the clone a *local* commit identity so a commit-enabled skill can `git commit`
/// inside the container — the mounted `.git/config` carries it, and the container's base
/// image has no global git identity. It is the deliberately recognizable [`SCSH_COMMIT_NAME`]
/// bot (see its docs). Best-effort; failures never abort the run. (Cherry-picking these
/// commits back preserves this author; your own identity becomes the committer.)
fn set_clone_identity(run_dir: &Path) {
  let _ = git_capture(run_dir, &["config", "user.email", SCSH_COMMIT_EMAIL]);
  let _ = git_capture(run_dir, &["config", "user.name", SCSH_COMMIT_NAME]);
}

/// Best-effort: create a local branch for each `origin/*` branch the host-side
/// clone already has, so `git branch` in the container lists them all without any
/// fetch inside the container. Failures here never abort the run — the full history
/// is already present either way.
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

// ---------------------------------------------------------------------------
// opencode credentials and config
//
// opencode in the container needs the host's login to talk to a model — especially
// custom/third-party providers configured in opencode (e.g. Nebius GLM). The image
// sets `XDG_DATA_HOME` to `repo/tmp/.xdg-data`. scsh copies the host's auth.json and opencode
// config (`~/.config/opencode/opencode.json`, optional `opencode.jsonc`) into each run clone
// under `tmp/.opencode-forward/` and bind-mounts from there — parallel runs cannot safely share
// one host bind-mount on Apple Containers. Opt out with `SCSH_NO_OPENCODE_AUTH=1`.
//
// Claude Code reads OAuth from `CLAUDE_CODE_OAUTH_TOKEN` (preferred — from `claude setup-token`)
// or `~/.claude/.credentials.json` (plus optional `~/.claude.json` / `~/.claude` config).
// scsh copies the host's Claude config into the run dir's gitignored tmp/ and bind-mounts
// it into the container; when the token env var is set it is also passed into the container
// and written as `.credentials.json` in the copy. Opt out with `SCSH_NO_CLAUDE_AUTH=1`.
// ---------------------------------------------------------------------------

/// Whether scsh forwards Claude credentials into runs (on unless opted out).
fn claude_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_CLAUDE_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh forwards opencode credentials into runs (on unless opted out).
fn opencode_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_OPENCODE_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh forwards codex credentials into runs (on unless opted out).
fn codex_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_CODEX_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh forwards grok credentials into runs (on unless opted out).
fn grok_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_GROK_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh forwards cursor credentials into runs (on unless opted out).
fn cursor_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_CURSOR_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh keeps every skill's `/tmp` run-clone instead of cleaning up. By default a
/// successful skill's clone is removed after the run (its result was collected and any commits
/// integrated) while a failed skill's clone is kept for inspection, and stale clones from past
/// runs are swept at startup. Set `SCSH_KEEP_RUNS=1` to keep all clones and skip the sweep.
fn keep_run_dirs() -> bool {
  matches!(std::env::var("SCSH_KEEP_RUNS").ok().as_deref(), Some("1") | Some("true"))
}

/// Tell the session-browser daemon to retry run-dir cleanup later if the client did not remove it.
fn schedule_run_dir_prune_backup(
  daemon_client: Option<&std::sync::Arc<daemon::Client>>, run_dir: &str, container_name: &str, runtime: &str,
  outcome_ok: bool,
) {
  if keep_run_dirs() {
    return;
  }
  if let Some(c) = daemon_client {
    c.schedule_run_dir_prune(run_dir, container_name, runtime, outcome_ok);
  }
}

/// Copy the host's opencode auth and config into `run_dir` for the upcoming run.
fn forward_opencode(run_dir: &Path) -> Option<PathBuf> {
  let home = std::env::var_os("HOME").map(PathBuf::from)?;
  let xdg_data = std::env::var_os("XDG_DATA_HOME");
  let xdg_config = std::env::var_os("XDG_CONFIG_HOME");
  let auth_src = runtime::opencode_auth_in(xdg_data.as_deref(), Some(home.as_os_str())).filter(|p| p.is_file())?;

  let root = run_dir.join(runtime::OPENCODE_FORWARD_REL);
  let xdg_dir = root.join("xdg/opencode");
  let cfg_dir = root.join("config/opencode");
  std::fs::create_dir_all(&xdg_dir).ok()?;
  std::fs::create_dir_all(&cfg_dir).ok()?;
  std::fs::copy(&auth_src, xdg_dir.join("auth.json")).ok()?;

  if let Some(cfg) = runtime::opencode_config_json_in(xdg_config.as_deref(), Some(home.as_os_str())) {
    std::fs::copy(&cfg, cfg_dir.join("opencode.json")).ok()?;
  }
  if let Some(cfg) = runtime::opencode_config_jsonc_in(xdg_config.as_deref(), Some(home.as_os_str())) {
    std::fs::copy(&cfg, cfg_dir.join("opencode.jsonc")).ok()?;
  }
  Some(root)
}

/// Ensure mount-point parents exist in the run clone for forwarded opencode files.
fn prepare_opencode_mount_dirs(run_dir: &Path) {
  let _ = std::fs::create_dir_all(run_dir.join(runtime::AGENT_XDG_DATA_REL).join("opencode"));
}

/// Copy the host's Claude config into `run_dir` for the upcoming run, returning the auth root
/// (so the caller can remove it afterward). Uses `CLAUDE_CODE_OAUTH_TOKEN` when set, else
/// `~/.claude/.credentials.json`, and copies `~/.claude` / `~/.claude.json` when present.
fn forward_claude_auth(run_dir: &Path) -> Option<PathBuf> {
  let home = std::env::var_os("HOME").map(PathBuf::from);
  let token = runtime::claude_oauth_token();
  let keychain_creds = runtime::claude_keychain_credentials_json();
  let host_claude = home.as_ref().filter(|h| h.join(".claude").is_dir());
  let host_json = home.as_ref().filter(|h| h.join(".claude.json").is_file());
  let host_creds = host_claude.as_ref().map(|h| h.join(".claude").join(".credentials.json")).filter(|p| p.is_file());

  if token.is_none() && keychain_creds.is_none() && host_creds.is_none() && host_claude.is_none() && host_json.is_none()
  {
    return None;
  }

  let root = run_dir.join(runtime::CLAUDE_AUTH_REL);
  let claude_dir = root.join(".claude");
  std::fs::create_dir_all(&claude_dir).ok()?;

  if let Some(h) = host_claude {
    copy_dir_all(&h.join(".claude"), &claude_dir).ok()?;
  }
  if let Some(h) = host_json {
    // With CLAUDE_CONFIG_DIR set (in the image), Claude Code reads its state json from
    // $CLAUDE_CONFIG_DIR/.claude.json — inside the copied dir, not the home root.
    std::fs::copy(h.join(".claude.json"), claude_dir.join(".claude.json")).ok()?;
  }
  // Credential preference: an already-copied host `.credentials.json` is complete; else
  // the macOS keychain blob is the same complete JSON (expiry, scopes, refresh token) —
  // required for the interactive TUI to consider itself logged in; else fall back to a
  // minimal file from the bare env token (sufficient for headless harnesses).
  if !claude_dir.join(".credentials.json").is_file() {
    if let Some(json) = &keychain_creds {
      let path = claude_dir.join(".credentials.json");
      std::fs::write(&path, json).ok()?;
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
      }
    } else if let Some(t) = &token {
      write_claude_credentials_file(&claude_dir, t)?;
    }
  }
  // The interactive TUI must not block on first-run dialogs: merge the onboarding /
  // bypass-consent / repo-trust keys into the copied state json (fresh file if none).
  seed_claude_tui_config(&claude_dir.join(".claude.json"));

  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(json) = claude_dir.join(".claude.json").canonicalize() {
      let _ = std::fs::set_permissions(&json, std::fs::Permissions::from_mode(0o600));
    }
    if let Ok(creds) = claude_dir.join(".credentials.json").canonicalize() {
      let _ = std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o600));
    }
  }
  Some(root)
}

/// Merge the keys that keep Claude Code's interactive TUI from blocking on first-run
/// dialogs — onboarding, bypass-permissions consent, and trust for the container repo
/// path — into the run's copied `.claude.json` ([`forward_claude_auth`]). A missing or
/// unparsable copy becomes a fresh minimal config, so the file always exists and mounts.
fn seed_claude_tui_config(json_path: &Path) {
  use crate::json::Value;
  fn set(obj: &mut Vec<(String, Value)>, key: &str, val: Value) {
    if let Some(slot) = obj.iter_mut().find(|(k, _)| k == key) {
      slot.1 = val;
    } else {
      obj.push((key.to_string(), val));
    }
  }
  let mut root = match std::fs::read_to_string(json_path).ok().and_then(|t| json::parse(&t).ok()) {
    Some(Value::Object(o)) => o,
    _ => Vec::new(),
  };
  set(&mut root, "hasCompletedOnboarding", Value::Bool(true));
  set(&mut root, "bypassPermissionsModeAccepted", Value::Bool(true));
  let repo_project = Value::Object(vec![
    ("hasTrustDialogAccepted".to_string(), Value::Bool(true)),
    ("hasCompletedProjectOnboarding".to_string(), Value::Bool(true)),
  ]);
  let merged_into_existing = match root.iter_mut().find(|(k, _)| k == "projects") {
    Some((_, Value::Object(projects))) => {
      set(projects, runtime::AGENT_REPO, repo_project.clone());
      true
    }
    _ => false,
  };
  if !merged_into_existing {
    set(&mut root, "projects", Value::Object(vec![(runtime::AGENT_REPO.to_string(), repo_project)]));
  }
  let _ = std::fs::write(json_path, json::write(&Value::Object(root)));
}

/// Copy the host's Codex auth/config into the run clone's `tmp/.codex` (the image's
/// `CODEX_HOME`), returning that dir so the caller can scrub the credentials afterward.
/// No bind-mounts needed: the tree rides along with the repo/tmp mount in both mount modes.
fn forward_codex(run_dir: &Path) -> Option<PathBuf> {
  let host_home = runtime::codex_home_on_host()?;
  let auth = host_home.join("auth.json");
  let config = host_home.join("config.toml");
  if !auth.is_file() && !config.is_file() {
    return None;
  }
  let dest = run_dir.join(runtime::CODEX_FORWARD_REL);
  std::fs::create_dir_all(&dest).ok()?;
  for name in ["auth.json", "config.toml"] {
    let src = host_home.join(name);
    if src.is_file() {
      std::fs::copy(&src, dest.join(name)).ok()?;
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest.join(name), std::fs::Permissions::from_mode(0o600));
      }
    }
  }
  Some(dest)
}

/// Remove forwarded Codex credentials from a run dir, keeping codex's session/log data
/// (useful when a failed run dir is kept for inspection — tokens must not linger in /tmp).
fn scrub_codex_credentials(codex_dir: &Path) {
  for name in ["auth.json", "config.toml"] {
    let _ = std::fs::remove_file(codex_dir.join(name));
  }
}

/// Copy the host's Grok auth/config into the run clone's `tmp/.grok` (the image's
/// `GROK_HOME`), returning that dir so the caller can scrub the credentials afterward.
/// Same pattern as codex: no bind-mounts needed in either repo mount mode.
fn forward_grok(run_dir: &Path) -> Option<PathBuf> {
  let host_home = runtime::grok_home_on_host()?;
  let auth = host_home.join("auth.json");
  if !auth.is_file() {
    return None;
  }
  let dest = run_dir.join(runtime::GROK_FORWARD_REL);
  std::fs::create_dir_all(&dest).ok()?;
  for name in ["auth.json", "config.toml", "user-settings.json"] {
    let src = host_home.join(name);
    if src.is_file() {
      std::fs::copy(&src, dest.join(name)).ok()?;
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest.join(name), std::fs::Permissions::from_mode(0o600));
      }
    }
  }
  Some(dest)
}

/// Remove forwarded Grok credentials from a run dir, keeping grok's session/log data.
fn scrub_grok_credentials(grok_dir: &Path) {
  for name in ["auth.json", "config.toml", "user-settings.json"] {
    let _ = std::fs::remove_file(grok_dir.join(name));
  }
}

/// Copy the host's Cursor config and OAuth tokens into the run clone's gitignored tmp/.
fn forward_cursor(run_dir: &Path) -> bool {
  let mut forwarded = false;
  if let Some(host_home) = runtime::cursor_home_on_host() {
    let dest = run_dir.join(runtime::CURSOR_FORWARD_REL);
    let mut any = false;
    for name in ["cli-config.json", "mcp.json"] {
      let src = host_home.join(name);
      if src.is_file() {
        if std::fs::create_dir_all(&dest).is_ok() {
          if std::fs::copy(&src, dest.join(name)).is_ok() {
            #[cfg(unix)]
            {
              use std::os::unix::fs::PermissionsExt;
              let _ = std::fs::set_permissions(dest.join(name), std::fs::Permissions::from_mode(0o600));
            }
            any = true;
          }
        }
      }
    }
    forwarded |= any;
  }
  let auth_dest = run_dir.join(runtime::CURSOR_AUTH_FORWARD_REL);
  if let Some(src) = runtime::cursor_auth_file_on_host() {
    if std::fs::create_dir_all(&auth_dest).is_ok() {
      if std::fs::copy(&src, auth_dest.join("auth.json")).is_ok() {
        #[cfg(unix)]
        {
          use std::os::unix::fs::PermissionsExt;
          let _ = std::fs::set_permissions(auth_dest.join("auth.json"), std::fs::Permissions::from_mode(0o600));
        }
        forwarded = true;
      }
    }
  } else if let Some(access) = runtime::cursor_keychain_access_token() {
    let refresh = runtime::cursor_keychain_refresh_token().unwrap_or_else(|| access.clone());
    if std::fs::create_dir_all(&auth_dest).is_ok() {
      let body = format!(r#"{{"accessToken":{},"refreshToken":{}}}"#, json::quote(&access), json::quote(&refresh));
      if std::fs::write(auth_dest.join("auth.json"), body).is_ok() {
        #[cfg(unix)]
        {
          use std::os::unix::fs::PermissionsExt;
          let _ = std::fs::set_permissions(auth_dest.join("auth.json"), std::fs::Permissions::from_mode(0o600));
        }
        forwarded = true;
      }
    }
  }
  forwarded
}

/// Remove forwarded Cursor credentials from a run dir, keeping session/log data.
fn scrub_cursor_credentials(run_dir: &Path) {
  for name in ["cli-config.json", "mcp.json"] {
    let _ = std::fs::remove_file(run_dir.join(runtime::CURSOR_FORWARD_REL).join(name));
  }
  let _ = std::fs::remove_file(run_dir.join(runtime::CURSOR_AUTH_FORWARD_REL).join("auth.json"));
}

fn write_claude_credentials_file(claude_dir: &Path, token: &str) -> Option<()> {
  let path = claude_dir.join(".credentials.json");
  let body = format!("{{\"claudeAiOauth\":{{\"accessToken\":{}}}}}", json::quote(token));
  std::fs::write(&path, body).ok()?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
  }
  Some(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
  std::fs::create_dir_all(dst)?;
  for entry in std::fs::read_dir(src)? {
    let entry = entry?;
    let path = entry.path();
    let dest = dst.join(entry.file_name());
    if entry.file_type()?.is_dir() {
      copy_dir_all(&path, &dest)?;
    } else {
      std::fs::copy(&path, &dest)?;
    }
  }
  Ok(())
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

/// Pull the skill's `result` file OUT of the run clone into the caller repo (host-side,
/// after the container exits). Moves any pre-existing host file aside to
/// `<name>.bak.YYYYMMDD-HHMMSS-utc` first. This is always done for every skill — unlike
/// commits, which are pulled out only when `commits: true` and the skill committed.
fn collect_skill_result(root: &Path, run_dir: &Path, result: &str, secs: u64) -> Result<String, String> {
  let produced = run_dir.join(result);
  if !produced.is_file() {
    let ctx = failure::missing_result_context(run_dir, result);
    return Err(format!("did not produce its result file '{result}'{ctx}"));
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

/// Run `git -C <dir> <args>` for its exit status only, swallowing its output (so a
/// cherry-pick conflict doesn't spill onto the terminal). `true` on success.
fn git_status_ok(dir: &std::path::Path, args: &[&str]) -> bool {
  Command::new("git").arg("-C").arg(dir).args(args).output().map(|o| o.status.success()).unwrap_or(false)
}

/// The caller repo's current branch name (for the "rebased onto <branch>" line);
/// falls back to "HEAD" when detached or unreadable.
fn current_branch(root: &Path) -> String {
  git_capture(root, &["rev-parse", "--abbrev-ref", "HEAD"])
    .map(|s| s.trim().to_string())
    .unwrap_or_else(|| "HEAD".into())
}

/// What happened when bringing a commit-enabled skill's commits back.
enum Integration {
  /// The commits were rebased (cherry-picked) onto the caller's current branch.
  Applied { count: usize },
  /// They didn't apply cleanly, so they were saved to a distinct branch instead;
  /// the caller's branch was left untouched.
  Saved { branch: String, count: usize },
}

/// Pull new commits OUT of a commit-enabled skill's run clone into the caller repo.
/// Called on the **host** after the container exits. Only when the skill declared
/// `commits: true` and actually added commits (`base..clone-HEAD` non-empty).
/// Uses `git fetch` from the **local run-clone path** — not from GitHub — then
/// cherry-picks onto the caller's current branch. Returns `None` when the skill added
/// no commits. scsh never pushes to any remote.
fn integrate_commits(
  root: &Path, run_dir: &Path, base: &str, skill: &str, stamp: &str,
) -> Result<Option<Integration>, String> {
  let source = runtime::commits_fetch_path(run_dir);
  // The skill's branch tip — what it left after (maybe) committing.
  let tip = match git_capture(&source, &["rev-parse", "HEAD"]) {
    Some(t) => t.trim().to_string(),
    None => return Err("could not read the clone's HEAD".into()),
  };
  if tip == base {
    return Ok(None); // the skill added nothing
  }
  // Make the skill's new objects available in the caller repo (host fetch from the local
  // run clone or pull.git bare repo — NOT from GitHub).
  let fetch_path = source.to_string_lossy();
  if !git_status_ok(root, &["fetch", "--no-tags", "--quiet", &fetch_path, "HEAD"]) {
    return Err("could not fetch the skill's commits from its clone".into());
  }
  let range = format!("{base}..{tip}");
  let count =
    git_capture(root, &["rev-list", "--count", &range]).and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(0);
  if count == 0 {
    return Ok(None);
  }
  // Try to rebase the range onto the caller's current branch. --keep-redundant-commits
  // preserves the side effect even if a commit's changes are already present (so the
  // "run twice = two commits" guarantee holds rather than collapsing to a no-op).
  if git_status_ok(root, &["cherry-pick", "--keep-redundant-commits", &range]) {
    Ok(Some(Integration::Applied { count }))
  } else {
    let _ = git_status_ok(root, &["cherry-pick", "--abort"]);
    let branch = incoming_branch_name(skill, stamp, &tip);
    if !git_status_ok(root, &["branch", "--force", &branch, &tip]) {
      return Err(format!("commits didn't rebase cleanly and the fallback branch '{branch}' could not be created"));
    }
    Ok(Some(Integration::Saved { branch, count }))
  }
}

/// A distinct branch name for commits that couldn't be rebased cleanly:
/// `scsh/incoming/<skill>-<stamp>-<short>` — the UTC stamp plus the tip's short hash,
/// so the user can see exactly what the branch carries.
fn incoming_branch_name(skill: &str, stamp: &str, tip: &str) -> String {
  let short: String = tip.chars().take(7).collect();
  format!("scsh/incoming/{}-{}-utc-{}", runtime::sanitize_component(skill), stamp, short)
}

// ---------------------------------------------------------------------------
// Result cache (content-addressed, under the repo's gitignored tmp/.sccache/)
// ---------------------------------------------------------------------------

/// Where cached results live: the repo's gitignored `tmp/.sccache/`.
fn cache_dir(root: &Path) -> PathBuf {
  root.join("tmp").join(".sccache")
}

/// The cache key for a skill run: a sha256 over a deterministic blob of the repo's
/// committed content (the HEAD tree hash), the skill's own files (`SKILL.md` + scripts,
/// each hashed, in sorted order), and the resolved env (sorted). So the **same commit +
/// same skill + same env** map to the same key. `None` when the repo content can't be
/// read (e.g. a repo with no commit yet) — then the run is simply not cached.
fn cache_key(root: &Path, skill: &ResolvedInvocation, env: &[(String, String)]) -> Option<String> {
  let tree = git_capture(root, &["rev-parse", "HEAD^{tree}"])?.trim().to_string();
  let mut blob = String::new();
  blob.push_str("scsh-cache v2\n");
  blob.push_str(&format!("repo-tree={tree}\n"));
  blob.push_str(&format!("invocation={}\n", skill.name));
  blob.push_str(&format!("skill={}\n", skill.skill_source));
  blob.push_str(&format!("harness={}\n", skill.harness.as_str()));
  blob.push_str(&format!("model={}\n", skill.model.as_deref().unwrap_or("")));
  blob.push_str(&format!("effort={}\n", skill.effort.as_deref().unwrap_or("")));
  blob.push_str("skill-files:\n");
  for (rel, hash) in skill_file_hashes(root, &skill.skill_source) {
    blob.push_str(&format!("{rel} {hash}\n"));
  }
  blob.push_str("env:\n");
  let mut pairs: Vec<&(String, String)> = env.iter().collect();
  pairs.sort_by(|a, b| a.0.cmp(&b.0));
  for (k, v) in pairs {
    blob.push_str(&format!("{k}={v}\n"));
  }
  Some(sha256::sha256_hex(blob.as_bytes()))
}

/// `(repo-relative path, sha256-of-content)` for every file under `.skills/<name>/`,
/// sorted by path — a deterministic fingerprint of the skill body and its scripts.
fn skill_file_hashes(root: &Path, name: &str) -> Vec<(String, String)> {
  let dir = root.join(".skills").join(name);
  let mut found: Vec<(String, PathBuf)> = Vec::new();
  collect_files(&dir, &dir, &mut found);
  found.sort();
  found.into_iter().map(|(rel, abs)| (rel, sha256::sha256_hex(&std::fs::read(&abs).unwrap_or_default()))).collect()
}

/// Recursively collect `(path-relative-to-base, absolute-path)` for every regular file
/// under `dir`. Order is not guaranteed (the caller sorts).
fn collect_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
  let entries = match std::fs::read_dir(dir) {
    Ok(e) => e,
    Err(_) => return,
  };
  for entry in entries.flatten() {
    let path = entry.path();
    if path.is_dir() {
      collect_files(base, &path, out);
    } else if path.is_file() {
      if let Ok(rel) = path.strip_prefix(base) {
        out.push((rel.to_string_lossy().replace('\\', "/"), path));
      }
    }
  }
}

/// A cached run: the skill's result-file content, and (for a commit-enabled skill) the
/// commits it made, journaled as a git `format-patch` mbox to replay on a hit.
struct CacheEntry {
  result: String,
  commits: Option<String>,
}

/// Look up the cache entry for `key` (the result content, plus any journaled commits).
fn cache_lookup(root: &Path, key: &str) -> Option<CacheEntry> {
  let text = std::fs::read_to_string(cache_dir(root).join(format!("{key}.json"))).ok()?;
  Some(CacheEntry { result: json::field(&text, "result")?, commits: json::field(&text, "commits") })
}

/// Store a skill's result-file `content` (and any commit `patch`) in the cache under `key`.
/// Best-effort: a write failure just means the next identical run won't be a hit.
fn cache_store(root: &Path, key: &str, content: &str, commits: Option<&str>) {
  let dir = cache_dir(root);
  if std::fs::create_dir_all(&dir).is_err() {
    return;
  }
  let entry = match commits {
    Some(patch) => format!("{{\"result\": {}, \"commits\": {}}}\n", json::quote(content), json::quote(patch)),
    None => format!("{{\"result\": {}}}\n", json::quote(content)),
  };
  let _ = std::fs::write(dir.join(format!("{key}.json")), entry);
}

/// A commit-enabled skill's new commits in its clone (`base..HEAD`) as a git `format-patch`
/// mbox, or `None` if it committed nothing. Stored in the cache so a hit can replay them.
fn commit_patch(clone: &Path, base: &str) -> Option<String> {
  let out = git_capture(clone, &["format-patch", &format!("{base}..HEAD"), "--stdout"])?;
  if out.trim().is_empty() {
    None
  } else {
    Some(out)
  }
}

/// Replay commits journaled in the cache (a `format-patch` mbox) onto the caller's current
/// branch via `git am`, so a cache hit reproduces the commit side effect. Returns `Applied`
/// on a clean replay; if the patch doesn't apply, aborts and saves it under tmp/.sccache for
/// the user to apply by hand (reported via the `Err` path), leaving the branch untouched.
fn apply_cached_commits(root: &Path, patch: &str, skill: &str, stamp: &str) -> Result<Option<Integration>, String> {
  let before = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
  let staged = std::env::temp_dir().join(format!("scsh-replay-{}-{stamp}.patch", std::process::id()));
  std::fs::write(&staged, patch).map_err(|_| "could not stage the cached commits".to_string())?;
  let applied = git_status_ok(root, &["am", "--keep-cr", &staged.to_string_lossy()]);
  let _ = std::fs::remove_file(&staged);
  if applied {
    let count = before
      .as_deref()
      .and_then(|b| git_capture(root, &["rev-list", "--count", &format!("{b}..HEAD")]))
      .and_then(|s| s.trim().parse::<usize>().ok())
      .unwrap_or(1);
    return Ok(Some(Integration::Applied { count }));
  }
  let _ = git_status_ok(root, &["am", "--abort"]);
  let saved = cache_dir(root).join(format!("incoming-{}-{stamp}.patch", runtime::sanitize_component(skill)));
  let _ = std::fs::create_dir_all(cache_dir(root));
  let _ = std::fs::write(&saved, patch);
  Err(format!(
    "cached commits didn't apply cleanly — saved the patch to {} (apply with: git am <file>)",
    saved.display()
  ))
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

/// Build a single harness image through the live board's build proc (so its output streams into the
/// collapsible build row, timestamped). docker/podman take the in-memory Dockerfile on stdin;
/// Apple's `container` has no stdin build mode, so it gets an ephemeral context dir instead.
fn run_build(
  build: &ui::screen::Proc, runtime_name: &str, tag: &str, target: &str, dockerfile: &str, uid: u32, gid: u32,
  fingerprint: &str,
) -> Result<(), (String, i32)> {
  let tz = runtime::host_timezone();
  let started = |e: std::io::Error| (format!("failed to start '{runtime_name}': {e}"), 1);
  let (ok, last) = match runtime::build_method(runtime_name) {
    runtime::BuildMethod::Stdin => {
      let cmd = runtime::build_command_stdin(runtime_name, tag, target, uid, gid, &tz, fingerprint);
      build.run_with_stdin(&cmd[0], &cmd[1..], dockerfile.as_bytes()).map_err(started)?
    }
    runtime::BuildMethod::ContextDir => {
      let dir = make_temp_dir().map_err(|e| (format!("could not create build context: {e}"), 1))?;
      let path = dir.join(runtime::CONTEXT_DOCKERFILE_NAME);
      if let Err(e) = std::fs::write(&path, dockerfile) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err((format!("could not write Dockerfile to build context: {e}"), 1));
      }
      let cmd =
        runtime::build_command_context(runtime_name, tag, target, &dir.to_string_lossy(), uid, gid, &tz, fingerprint);
      let out = build.run(&cmd[0], &cmd[1..]).map_err(started);
      let _ = std::fs::remove_dir_all(&dir); // best-effort cleanup
      out?
    }
  };
  if ok {
    Ok(())
  } else {
    let tail = build.tail_lines(failure::FAILURE_TAIL_LINES);
    let excerpt = failure::failure_excerpt(last.as_deref(), &tail, "build produced no output");
    Err((format!("image build failed (runtime={runtime_name}, target={target}, tag={tag}): {excerpt}"), 1))
  }
}

fn make_temp_dir() -> std::io::Result<PathBuf> {
  let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
  let dir = std::env::temp_dir().join(format!("scsh-build-{}-{nanos}", std::process::id()));
  std::fs::create_dir_all(&dir)?;
  Ok(dir)
}

fn print_build_command(
  runtime_name: &str, tag: &str, target: &str, _dockerfile: &str, uid: u32, gid: u32, tz: &str, fingerprint: &str,
) {
  match runtime::build_method(runtime_name) {
    runtime::BuildMethod::Stdin => {
      let build = runtime::build_command_stdin(runtime_name, tag, target, uid, gid, tz, fingerprint);
      println!("{}", runtime::shell_join(&build));
    }
    runtime::BuildMethod::ContextDir => {
      let ctx = std::env::temp_dir().join("scsh-build-XXXXXX");
      let build =
        runtime::build_command_context(runtime_name, tag, target, &ctx.to_string_lossy(), uid, gid, tz, fingerprint);
      println!("{}", runtime::shell_join(&build));
      println!("{}", h_dim("# in-memory Dockerfile written to an ephemeral context dir"));
    }
  }
}

fn init_demo() -> i32 {
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return 1;
  }
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return 1;
    }
  };
  let path = root.join(".scsh.yml");
  if path.exists() {
    fail(&format!(".scsh.yml already exists at {} — not overwriting", path.display()));
    hint("delete it first if you want a fresh demo config");
    return 1;
  }
  if let Err(e) = std::fs::write(&path, config::demo_yaml()) {
    fail(&format!("could not write {}: {e}", path.display()));
    return 1;
  }
  ok(&format!("wrote demo config to {}", path.display()));

  // Leave the repo runnable right away: a real `scsh` run refuses to proceed unless
  // the repo's /tmp is gitignored (build scratch and result copies must stay
  // untracked). Set that up now so the next `scsh` clears the guard instead of
  // bouncing off it.
  match ensure_tmp_gitignored(&root) {
    Ok(true) => ok("added '/tmp' to .gitignore (keeps build scratch and result copies untracked)"),
    Ok(false) => {} // already ignored — nothing to change
    Err(e) => hint(&format!("could not update .gitignore automatically ({e}); add a '/tmp' line yourself")),
  }

  // Scaffold the example skills so the demo repo has something real to run. Never
  // overwrite an existing skill file. Track what we wrote so it can be committed.
  let mut skill_paths: Vec<String> = Vec::new();
  for (rel, body, executable) in config::demo_skills() {
    let dest = root.join(rel);
    if dest.exists() {
      hint(&format!("kept existing {rel} (not overwritten)"));
      continue;
    }
    if let Some(parent) = dest.parent() {
      if let Err(e) = std::fs::create_dir_all(parent) {
        hint(&format!("could not create {}: {e}", parent.display()));
        continue;
      }
    }
    match std::fs::write(&dest, body) {
      Ok(()) => {
        // Scripts a skill ships are run directly by the harness, so make them executable.
        #[cfg(unix)]
        if executable {
          use std::os::unix::fs::PermissionsExt;
          let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
        }
        skill_paths.push(rel.to_string());
      }
      Err(e) => hint(&format!("could not write {}: {e}", dest.display())),
    }
  }
  if !skill_paths.is_empty() {
    let n = skill_paths.len();
    ok(&format!("scaffolded {n} example-skill file{} under .skills/", if n == 1 { "" } else { "s" }));
  }

  // Wire up skill discovery the way this repo's convention does (see
  // .skills/README.md): a harness looks for project skills in its OWN dir — none of
  // them know about `.skills/` — so scsh keeps the skills in `.skills/` and symlinks
  // each harness dir to it. That's what lets the opencode harness (and any other)
  // find them; committed with the project, so the links survive the clone scsh
  // mounts into the container.
  let links = link_skill_hosts(&root);
  if !links.is_empty() {
    let n = links.len();
    ok(&format!(
      "linked {n} harness skill dir{} → .skills (so the harness finds the skills)",
      if n == 1 { "" } else { "s" }
    ));
  }

  // Initialize the project *fully*: commit the scaffold so the working tree is clean
  // and the very next `scsh` runs (a real run clones COMMITTED state and refuses a
  // dirty tree). Stage only what we created — never `git add -A` — so any unrelated
  // work already in the repo is left untouched.
  let mut staged = vec![".scsh.yml".to_string()];
  if root.join(".gitignore").exists() {
    staged.push(".gitignore".to_string());
  }
  staged.extend(skill_paths);
  staged.extend(links);
  match commit_scaffold(&root, &staged) {
    Ok(()) => {
      ok("committed the scaffold");
      let remaining = uncommitted_changes(&root);
      if remaining.is_empty() {
        println!("\nThe project is committed and clean. Next:");
        println!("  {}   {}", bold("scsh run"), h_dim("#  build the image and run the .scsh.yml skills in parallel"));
      } else {
        // We committed the scaffold, but the repo had other uncommitted changes; a
        // real run needs a fully clean tree, so point those out too.
        fail("the repo still has uncommitted changes — a real run needs a clean working tree");
        hint(&format!("commit or stash them, then run {}:", bold("scsh")));
        hint(&format!("{}", bold("git add -A && git commit -m \"wip\"")));
      }
    }
    Err(e) => {
      hint(&format!("couldn't commit the scaffold automatically ({e})"));
      println!("\nNext: commit the scaffold, then run 'scsh' (a run clones committed state):");
      println!("  {}", bold("git add -A && git commit -m \"add scsh demo project\""));
      println!("  {}   {}", bold("scsh run"), h_dim("#  build the image and run the .scsh.yml skills in parallel"));
    }
  }
  print_skill_usage();
  0
}

/// Commit the freshly-scaffolded project so the working tree is clean and the very
/// next `scsh` can run (a real run clones COMMITTED state). Stages only `paths`
/// (never `git add -A`), so unrelated work already in the repo is left untouched.
/// `Err` carries git's message when nothing can be committed or git refuses (e.g.
/// no `user.name`/`user.email` configured) — init then tells the user to commit.
fn commit_scaffold(root: &Path, paths: &[String]) -> Result<(), String> {
  let add = Command::new("git").arg("-C").arg(root).arg("add").arg("--").args(paths).output();
  let add = add.map_err(|e| format!("git add: {e}"))?;
  if !add.status.success() {
    return Err(String::from_utf8_lossy(&add.stderr).trim().to_string());
  }
  let out = Command::new("git")
    .arg("-C")
    .arg(root)
    .args(["commit", "-q", "-m", "Add scsh demo project (config + skills)"])
    .output()
    .map_err(|e| format!("git commit: {e}"))?;
  if out.status.success() {
    Ok(())
  } else {
    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
  }
}

/// Per-file outcome of an install: how many skill files were newly written, replaced (by
/// `updateskills`), already identical, or kept because they differ from the source.
#[derive(Default)]
struct InstallCounts {
  installed: u32,
  updated: u32,
  already: u32,
  /// Repo-relative paths kept untouched because they differ from the source.
  differing: Vec<String>,
}

impl InstallCounts {
  /// Fold another install's tallies into this one — so installing several source repos in one
  /// command reports a single combined summary.
  fn merge(&mut self, other: InstallCounts) {
    self.installed += other.installed;
    self.updated += other.updated;
    self.already += other.already;
    self.differing.extend(other.differing);
  }
}

/// Install skills into the current repo's `.skills/` plus the harness discovery symlinks.
/// With no `sources`, installs scsh's own bundled skill (see [`config::bundled_skills`]); with
/// one or more `sources` (git URLs or local paths), clones each and installs the
/// `.skills/<name>/` skills it ships, in order — as if the command were run once per repo.
/// `overwrite` (the `updateskills` command) replaces existing files; otherwise an identical
/// file is "already installed", and a differing one is kept untouched. Like a real run, this
/// requires a clean working tree (so the install is a reviewable diff) and ensures `/tmp` is
/// gitignored before writing anything.
fn install_skills(overwrite: bool, sources: &[String]) -> i32 {
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return 1;
  }
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return 1;
    }
  };

  // Install into a clean tree (like a real run), so the install lands as ONE reviewable diff —
  // never silently mixed into unrelated uncommitted work. With several source repos this is
  // checked once, up front, before any of them are installed.
  let dirty = uncommitted_changes(&root);
  if !dirty.is_empty() {
    fail(
      "working tree has uncommitted changes — commit or stash them so the install lands as a clean, reviewable diff",
    );
    let shown = dirty.len().min(10);
    for p in &dirty[..shown] {
      hint(&format!("uncommitted: {p}"));
    }
    if dirty.len() > shown {
      hint(&format!("\u{2026}and {} more", dirty.len() - shown));
    }
    hint(&format!("commit or stash them first, then re-run:  {}", bold("git add -A && git commit -m \"wip\"")));
    return 1;
  }
  // Make the repo run-ready: installed skills write their result + cache under the repo's tmp/,
  // so ensure it's gitignored (append-only, exactly as init-demo-project does).
  match ensure_tmp_gitignored(&root) {
    Ok(true) => ok("added '/tmp' to .gitignore (keeps skill results + cache untracked)"),
    Ok(false) => {}
    Err(e) => hint(&format!("could not update .gitignore automatically ({e}); add a '/tmp' line yourself")),
  }

  // No source → scsh's bundled skill; otherwise install each source repo in order, accumulating
  // the per-file tallies so the final summary covers the whole command.
  let mut counts = InstallCounts::default();
  if sources.is_empty() {
    counts.merge(install_bundled(&root, overwrite));
  } else {
    for url in sources {
      match install_from_repo(&root, overwrite, url) {
        Ok(c) => counts.merge(c),
        Err(code) => return code,
      }
    }
  }

  let InstallCounts { installed, updated, already, differing } = counts;
  if installed > 0 {
    ok(&format!("installed {installed} skill file{} under .skills/", plural(installed as usize)));
  }
  if updated > 0 {
    ok(&format!("updated {updated} skill file{}", plural(updated as usize)));
  }
  if already > 0 {
    ok(&format!("{already} skill file{} already installed (identical)", plural(already as usize)));
  }
  for rel in &differing {
    hint(&format!("kept your modified {rel} (it differs from the source)"));
  }
  if !differing.is_empty() {
    let cmd = if sources.is_empty() {
      "scsh updateskills".to_string()
    } else {
      format!("scsh updateskills {}", sources.join(" "))
    };
    hint(&format!("to replace them with the source's version, run: {}", bold(&cmd)));
  }

  // Wire up the harness discovery dirs (.opencode/.claude/.cursor/.agents/.codex →
  // ../.skills), exactly as --init-demo-project does; existing ones are left alone.
  let links = link_skill_hosts(&root);
  if !links.is_empty() {
    ok(&format!("linked {} harness skill dir{} → .skills", links.len(), if links.len() == 1 { "" } else { "s" }));
  }
  if installed == 0 && updated == 0 && already == 0 && differing.is_empty() && links.is_empty() {
    ok("skills already installed; nothing to do");
  }
  // With no URL, all you get is scsh's bundled demo/self-test skill — point users at a real
  // skills repo for anything else.
  if sources.is_empty() {
    hint("that's scsh's bundled demo/self-test — run /scsh-harness-demo-and-selftest to exercise scsh end to end");
    hint(&format!(
      "for real skills, point me at a repo, e.g. {}",
      bold("scsh installskills https://github.com/dkorolev/beautiful-skills")
    ));
  }
  0
}

/// Install scsh's own skills, embedded in the binary at build time.
fn install_bundled(root: &Path, overwrite: bool) -> InstallCounts {
  let mut c = InstallCounts::default();
  for (rel, body) in config::bundled_skills() {
    write_one(&root.join(rel), body.as_bytes(), rel, overwrite, &mut c);
  }
  c
}

/// Header for a `.scsh.yml` that `installskills` creates from scratch in a consumer repo
/// (when it has none yet). The merged skill entries follow the `skills:` line.
const CONSUMER_MANIFEST_HEADER: &str = "\
# .scsh.yml — Scoped Skills Helper. Skills below were added by `scsh installskills`.
# The whole file is just your skills; scsh builds them on a built-in base image.
# Run `scsh help .scsh.yml` for the schema, or `scsh help` for commands.
skills:
";

/// Skills whose name begins with this prefix are authoring-only by convention: scsh never
/// installs them into a consumer repo — the same effect as `autoinstall: false`, but
/// self-evident in the name. Used for a repo's own meta/self-check skills.
const INTERNAL_PREFIX: &str = "internal-";

/// Clone `url` (shallow) and install its skills. If the source ships a `.scsh.yml`, that
/// manifest drives the install (only its listed skills, minus the authoring-only ones —
/// `autoinstall: false` or named `internal-*` — are installed, and each newly-installed
/// skill's entry is merged into the consumer's own `.scsh.yml`); otherwise every
/// `.skills/<name>/` directory is installed (still skipping `internal-*`). Returns
/// `Err(code)` on a clone failure, an invalid source manifest, or no installable skills.
fn install_from_repo(root: &Path, overwrite: bool, url: &str) -> Result<InstallCounts, i32> {
  let clone = std::env::temp_dir().join(format!("scsh-installskills-{}-{}", std::process::id(), now_secs()));
  let _ = std::fs::remove_dir_all(&clone); // clear any stale dir from a crashed run
  let cloned = Command::new("git")
    .args(["clone", "--depth", "1", url])
    .arg(&clone)
    .output()
    .map(|o| o.status.success())
    .unwrap_or(false);
  if !cloned {
    fail(&format!("could not clone {url}"));
    hint("check the URL and your network/credentials, then try again");
    let _ = std::fs::remove_dir_all(&clone);
    return Err(1);
  }

  let manifest = clone.join(".scsh.yml");
  let result = if manifest.is_file() {
    install_from_manifest(root, overwrite, url, &clone, &manifest)
  } else {
    install_all_skill_dirs(root, overwrite, url, &clone)
  };
  let _ = std::fs::remove_dir_all(&clone);
  result
}

/// Install every `.skills/<name>/` directory in the clone — the behavior when the source
/// ships no `.scsh.yml`. No manifest entries are merged (there is no manifest to read).
fn install_all_skill_dirs(root: &Path, overwrite: bool, url: &str, clone: &Path) -> Result<InstallCounts, i32> {
  let mut c = InstallCounts::default();
  let mut names: Vec<String> = Vec::new();
  let mut skipped: Vec<String> = Vec::new();
  if let Ok(entries) = std::fs::read_dir(clone.join(".skills")) {
    // A skill is a `.skills/<name>/` directory containing a SKILL.md.
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect();
    dirs.sort();
    for dir in dirs {
      let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
      if name.starts_with(INTERNAL_PREFIX) {
        skipped.push(name); // authoring-only by the `internal-` naming convention
        continue;
      }
      copy_skill_dir(root, &dir, &name, overwrite, &mut c);
      names.push(name);
    }
  }
  if names.is_empty() {
    fail(&format!("no skills found in {url} (expected .skills/<name>/SKILL.md)"));
    return Err(1);
  }
  ok(&format!("from {url}: {} skill{} — {}", names.len(), plural(names.len()), names.join(", ")));
  if !skipped.is_empty() {
    ok(&format!("skipped {} authoring-only (internal-*): {}", skipped.len(), skipped.join(", ")));
  }
  Ok(c)
}

/// Install from a source that ships a `.scsh.yml`: validate it (failing on a bad schema),
/// install every listed skill except those marked `autoinstall: false` (skills not listed
/// at all are skipped — the manifest is the shipping list), and merge each newly-installed
/// skill's entry, verbatim, into the consumer's own `.scsh.yml` so `scsh run` (default
/// skills) and `scsh run --profile <p>` pick them up immediately.
fn install_from_manifest(
  root: &Path, overwrite: bool, url: &str, clone: &Path, manifest: &Path,
) -> Result<InstallCounts, i32> {
  let src_text = match std::fs::read_to_string(manifest) {
    Ok(t) => t,
    Err(e) => {
      fail(&format!("{url}: could not read its .scsh.yml: {e}"));
      return Err(1);
    }
  };
  let cfg = match config::validate(&src_text) {
    Ok(c) => c,
    Err(errs) => {
      fail(&format!("{url}: its .scsh.yml does not match the schema ({} problem{})", errs.len(), plural(errs.len())));
      for e in &errs {
        hint(e);
      }
      return Err(1);
    }
  };

  // The consumer's existing manifest (if any) tells us which skills are already declared,
  // so we only append genuinely new entries and never clobber the user's edits.
  let local_path = root.join(".scsh.yml");
  let local_text = std::fs::read_to_string(&local_path).unwrap_or_default();
  let existing: std::collections::BTreeSet<String> =
    config::validate(&local_text).map(|c| c.skills.into_iter().map(|s| s.name).collect()).unwrap_or_default();

  let mut c = InstallCounts::default();
  let mut installed: Vec<String> = Vec::new();
  let mut skipped: Vec<String> = Vec::new();
  let mut added: Vec<String> = Vec::new();
  let mut conflicts: Vec<String> = Vec::new();
  let mut append = String::new();
  for skill in &cfg.skills {
    // Authoring-only skills are not installed: either marked `autoinstall: false`, or named
    // with the `internal-` convention (a self-documenting "internal to this repo" marker).
    if !skill.autoinstall || skill.name.starts_with(INTERNAL_PREFIX) {
      skipped.push(skill.name.clone());
      continue;
    }
    let dir = clone.join(".skills").join(&skill.name);
    if !dir.join("SKILL.md").is_file() {
      hint(&format!("{}: listed in .scsh.yml but has no .skills/{}/SKILL.md — skipped", skill.name, skill.name));
      continue;
    }
    copy_skill_dir(root, &dir, &skill.name, overwrite, &mut c);
    if !installed.contains(&skill.name) {
      installed.push(skill.name.clone());
    }
    if existing.contains(&skill.name) {
      conflicts.push(skill.name.clone());
      continue;
    }
    if let Some(block) = config::extract_skill_block(&src_text, &skill.name) {
      append.push_str(&block);
      added.push(skill.name.clone());
    }
  }

  if installed.is_empty() {
    fail(&format!("{url}: its .scsh.yml lists no installable skills (all authoring-only or missing)"));
    return Err(1);
  }
  ok(&format!("from {url}: {} skill{} — {}", installed.len(), plural(installed.len()), installed.join(", ")));
  if !skipped.is_empty() {
    ok(&format!("skipped {} authoring-only (autoinstall: false or internal-*): {}", skipped.len(), skipped.join(", ")));
  }
  if !conflicts.is_empty() {
    for name in &conflicts {
      hint(&format!("kept your existing '{name}' entry in .scsh.yml (conflicts with source manifest)"));
    }
  }

  // Merge the new entries into the consumer's .scsh.yml (append-only — existing entries
  // are left untouched), but only if the result still validates.
  if append.is_empty() {
    if conflicts.is_empty() {
      ok("the installed skills were already declared in .scsh.yml");
    }
  } else {
    let merged = if local_text.trim().is_empty() {
      format!("{CONSUMER_MANIFEST_HEADER}{append}")
    } else {
      let mut t = local_text.clone();
      if !t.ends_with('\n') {
        t.push('\n');
      }
      t.push_str(&append);
      t
    };
    if config::validate(&merged).is_ok() && write_file(&local_path, merged.as_bytes()) {
      ok(&format!("added {} skill{} to .scsh.yml: {}", added.len(), plural(added.len()), added.join(", ")));
    } else {
      hint(
        "installed the skill files, but merging them into .scsh.yml would make it invalid — left .scsh.yml unchanged",
      );
      hint(&format!("add by hand: {}", added.join(", ")));
    }
  }
  Ok(c)
}

/// Copy one skill directory (every file under it) from `src` into `root/.skills/<name>/`,
/// applying the per-file install rules.
fn copy_skill_dir(root: &Path, src: &Path, name: &str, overwrite: bool, c: &mut InstallCounts) {
  let dest_dir = root.join(".skills").join(name);
  let mut files = Vec::new();
  collect_files(src, src, &mut files);
  files.sort();
  for (rel, abs) in files {
    if let Ok(body) = std::fs::read(&abs) {
      write_one(&dest_dir.join(&rel), &body, &format!(".skills/{name}/{rel}"), overwrite, c);
    }
  }
}

/// Apply the install rules for one file: write if new, replace if `overwrite`, count as
/// already-installed if identical, or keep it (recording `shown`) if it differs.
fn write_one(dest: &Path, body: &[u8], shown: &str, overwrite: bool, c: &mut InstallCounts) {
  if dest.is_file() {
    let same = std::fs::read(dest).map(|d| d == body).unwrap_or(false);
    if same {
      c.already += 1;
    } else if overwrite {
      if write_file(dest, body) {
        c.updated += 1;
      }
    } else {
      c.differing.push(shown.to_string());
    }
  } else if write_file(dest, body) {
    c.installed += 1;
  }
}

/// Write a file, creating its parent dir. Reports and returns false on error.
fn write_file(dest: &Path, body: &[u8]) -> bool {
  if let Some(parent) = dest.parent() {
    if let Err(e) = std::fs::create_dir_all(parent) {
      hint(&format!("could not create {}: {e}", parent.display()));
      return false;
    }
  }
  match std::fs::write(dest, body) {
    Ok(()) => true,
    Err(e) => {
      hint(&format!("could not write {}: {e}", dest.display()));
      false
    }
  }
}

/// Symlink each harness's project skill-discovery dir at this repo's `.skills/`,
/// following the repo convention (see `.skills/README.md`): a harness reads skills
/// from its own dir — `.opencode/skills`, `.claude/skills`, `.cursor/skills`,
/// `.agents/skills`, `.codex/skills` — and none know about `.skills/`, so each is a
/// relative symlink (`../.skills`) to the one place the skills actually live. An
/// existing path (real dir or symlink) is left untouched. Returns how many it made.
#[cfg(unix)]
fn link_skill_hosts(root: &Path) -> Vec<String> {
  const HOSTS: &[&str] = &[".opencode/skills", ".claude/skills", ".cursor/skills", ".agents/skills", ".codex/skills"];
  let mut made = Vec::new();
  for host in HOSTS {
    let link = root.join(host);
    if link.symlink_metadata().is_ok() {
      continue; // already present — leave it
    }
    let linked = link.parent().map(|p| std::fs::create_dir_all(p).is_ok()).unwrap_or(false)
      && std::os::unix::fs::symlink("../.skills", &link).is_ok();
    if linked {
      made.push((*host).to_string());
    }
  }
  made
}

#[cfg(not(unix))]
fn link_skill_hosts(_root: &Path) -> Vec<String> {
  Vec::new()
}

/// Show how to run the scaffolded example skills.
fn print_skill_usage() {
  println!("\nThe demo .scsh.yml runs `add` on two routes by default (opencode+GPT, claude+Sonnet);");
  println!("`multiply` (X * Y) lives in the `multiply` profile because it REQUIRES X");
  println!("and Y. scsh resolves the env you forward (or refuses the skill). Examples — successes");
  println!("({}) and the intended refusal scsh guards against ({}):", ok_mark(), refused_mark());
  println!();
  example("scsh run", "add with defaults A=2 B=3 -> 2 + 3 = 5", true);
  example("A=10 B=20 scsh run", "add forwards your A,B -> 10 + 20 = 30", true);
  example("X=6 Y=7 scsh run --profile multiply", "also runs multiply -> 6 * 7 = 42", true);
  example("scsh run --profile multiply", "multiply REFUSED — X is required by ${X}", false);
  println!();
  let (var, def, req) = (env_syntax("${VAR}"), env_syntax("${VAR:-default}"), env_syntax("${VAR:?msg}"));
  println!("The env syntax: {var} requires VAR, {def} injects a default, {req}");
  println!("requires it with your message, and a bare literal is just that literal.");
  println!("When a skill finishes, scsh prints the message from its JSON result file (e.g.");
  println!("\"6 * 7 = 42\"), not just the file path. Preview the resolved env without containers:");
  println!("  {}      (shows every skill and the profile that runs it).", bold("scsh list"));
}

/// An env-syntax token (e.g. `${VAR}`), in cyan to set it apart from the prose.
fn env_syntax(token: &str) -> console::StyledObject<&str> {
  console::style(token).cyan()
}

/// A green ✓ for an example that works.
fn ok_mark() -> console::StyledObject<&'static str> {
  console::style("\u{2713}").green()
}

/// A grey ✗ for an example scsh intentionally refuses — it's the expected guardrail, not an
/// error, so it reads dim rather than alarming red.
fn refused_mark() -> console::StyledObject<&'static str> {
  console::style("\u{2717}").dim()
}

/// One example line: `  <command>  #  <comment>  <✓|✗>` — the command bold, the comment dimmed
/// (its `${…}` tokens cyan), and the mark green for a success or grey for an intended refusal.
fn example(cmd: &str, comment: &str, ok: bool) {
  let mark = if ok { ok_mark() } else { refused_mark() };
  let cmd = console::style(format!("{cmd:<35}")).bold();
  println!("  {cmd} {}  {mark}", dim_comment(comment));
}

/// Render a `#  <comment>` for an example line: dimmed, but with any `${…}` token in cyan so the
/// env syntax stands out even inside the comment.
fn dim_comment(comment: &str) -> String {
  let mut out = format!("{}", h_dim("#  "));
  let mut rest = comment;
  while let Some(start) = rest.find("${") {
    if let Some(end) = rest[start..].find('}') {
      let end = start + end + 1; // include the '}'
      out.push_str(&format!("{}", h_dim(&rest[..start])));
      out.push_str(&format!("{}", env_syntax(&rest[start..end])));
      rest = &rest[end..];
      continue;
    }
    break;
  }
  out.push_str(&format!("{}", h_dim(rest)));
  out
}

/// Ensure the repo ignores its `/tmp` (repo-root) path, appending a `/tmp` rule to
/// `<root>/.gitignore` when it isn't already ignored (creating the file if needed).
/// Returns whether a rule was added (`false` = already ignored, nothing changed).
/// It only ever **appends** — existing `.gitignore` content is never rewritten.
fn ensure_tmp_gitignored(root: &std::path::Path) -> Result<bool, String> {
  if tmp_is_gitignored(root) {
    return Ok(false);
  }
  let path = root.join(".gitignore");
  let mut content = match std::fs::read_to_string(&path) {
    Ok(s) => s,
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
    Err(e) => return Err(format!("could not read {}: {e}", path.display())),
  };
  if !content.is_empty() && !content.ends_with('\n') {
    content.push('\n');
  }
  content.push_str("# scsh uses the system temp dir for build scratch; never track a local /tmp.\n/tmp\n");
  std::fs::write(&path, content).map_err(|e| format!("could not write {}: {e}", path.display()))?;
  Ok(true)
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

/// A warning that isn't a hard failure — e.g. a skill's commits were saved to a branch
/// because they couldn't be rebased cleanly. Yellow `⚠`, so it stands apart from ✓/✗.
fn warn(msg: &str) {
  eprintln!("{} {msg}", console::style("\u{26a0}").yellow().bold().for_stderr());
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

/// Indented continuation line for a multi-line help entry (aligns under the description).
fn help_cont(desc: &str) {
  println!("      {}", h_dim(desc));
}

fn print_help(topic: HelpTopic) {
  match topic {
    HelpTopic::Overview => print_help_overview(),
    HelpTopic::Run => print_help_run(),
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
  help_row("run [profile…]", "Build the image; run skills in parallel.");
  help_cont("See `scsh help run` for profiles, preflight, and exit codes.");
  help_row("list (ls)", "List skills by profile (--verbose, --json).");
  help_row("check-profile <name>", "Exit 0 when the profile exists and has skills.");
  help_row("init-demo-project", "Scaffold and commit a demo project.");
  help_row("installskills [url…]", "Install skills (bundled or from git URLs).");
  help_row("updateskills [url…]", "Reinstall skills, overwriting local copies.");
  help_row("daemon", "start | stop | restart | status");
  help_cont("Browse run output at http://127.0.0.1:7274 (override: SCSH_DAEMON_PORT).");
  help_row("failures", "Browse the failure log (--session, --skill, --reason, --last, --stats).");
  help_row("stats", "Durations & workload per skill/route (--skill, --profile, --harness, --model, --raw).");
  help_row("prune [--now]", "Show the run-dir cleanup queue; --now forces a pass.");
  help_row("version", "Print the version (with the build's git hash).");
  help_row("help [topic]", "Show this help, or one of the topics below.");
  println!();
  println!("{}", h_head("More help:"));
  help_row("scsh help run", "How to run skills: profiles, preflight, exit codes, env vars.");
  help_row("scsh help .scsh.yml", "The project config file: every field + env syntax.");
  help_row("scsh help internals", "How a run works: clone, containers, auth, live board.");
  help_row("scsh help cache", "How results are cached, and when a re-run is a hit.");
  println!();
  println!("{}", h_head("Options:"));
  help_row("--profile <names>", "With `run`: only these profiles (`default` = no-profile skills).");
  help_row("--verbose", "With list: also print the Dockerfile and exact commands.");
  help_row("--json", "With list: print profiles and skills as JSON.");
  println!();
  println!("{}", h_dim("`run` bakes a dev toolchain into the image (python3/uv, Go, Rust, gh, aws, gcloud,"));
  println!("{}", h_dim("kubectl, psql, protoc, \u{2026}; no Java) and builds it with this machine's timezone."));
  println!("{}", h_dim("Full toolchain list: scsh help internals."));
  println!();
  println!("{} {}", h_dim("Aliases:"), h_dim("--help/-h \u{00b7} --version/-V \u{00b7} --init-demo-project"));
  println!();
}

/// `scsh help run` — how to invoke `run`, for humans and agents.
fn print_help_run() {
  println!();
  println!("{} {}", h_head("run"), console::style("\u{2014} run scoped skills in parallel").bold());
  println!();
  println!("{}", h_head("Synopsis"));
  println!("{}", h_dim("  scsh run [profile…]"));
  println!();
  println!("{}", h_head("Discover what to run (before `run`)"));
  help_row("scsh list", "Every skill by profile — result path, harness, env (human-readable).");
  help_row("scsh list --json", "Same, as JSON — preferred for scripts and agents.");
  help_row("scsh check-profile <name>", "Exit 0 iff that profile exists with at least one skill (no runtime).");
  println!();
  println!("{}", h_head("Profile selection"));
  println!("{}", h_dim("  Skills with no `profile:` belong to the reserved `default` profile."));
  println!("{}", h_dim("  A skill with `profile: X` runs only when you select profile X."));
  help_row("scsh run", "Run `default` only (skills with no profile).");
  help_row("scsh run code-review", "Run one named profile.");
  help_row("scsh run a b", "Run several profiles (same as `scsh run --profile a,b`).");
  help_row("scsh run --profile a,b", "Comma/semicolon-separated profile list.");
  println!("{}", h_dim("  If every skill is profiled, bare `scsh run` is a no-op that lists profiles."));
  println!();
  println!("{}", h_head("Preflight (fails fast — message names one fix)"));
  print!(
    r#"    1. git is installed
    2. current directory is inside a git repository
    3. working tree is clean          (scsh clones COMMITTED state only)
    4. .scsh.yml exists and matches the schema
    5. tmp/ is gitignored             (build scratch + results stay untracked)
    6. a container runtime is available (macOS: container → docker → podman;
       otherwise docker → podman; override with SCSH_RUNTIME=<name>)
    7. the runtime engine is running  (scsh prints how to start it)
"#
  );
  println!();
  println!("{}", h_head("What `run` does (summary)"));
  println!("{}", h_dim("  Builds one image per harness needed (`scsh-opencode`, `scsh-claude`), then runs"));
  println!("{}", h_dim("  every selected skill in parallel — each in its own container. On Linux/docker/podman"));
  println!("{}", h_dim("  the run dir is bind-mounted at /home/agent/repo; on macOS Apple Container scsh"));
  println!("{}", h_dim("  git-pushes into a bare repo and the container clones via a local git daemon."));
  println!("{}", h_dim("  Skills must not git fetch/pull remotes inside. After exit, scsh copies each"));
  println!("{}", h_dim("  Skills with `commits: true` may also bring commits back via local cherry-pick."));
  println!(
    "{}",
    h_dim(
      "  Unavailable harnesses and opencode models are skipped; \
the run fails only when every selected skill is skipped.",
    )
  );
  println!();
  println!("{}", h_head("Exit codes"));
  help_row("0", "Every selected skill that ran finished successfully (skipped harnesses are OK).");
  help_row("non-zero", "At least one skill failed, or every selected skill was skipped/unavailable.");
  println!();
  println!("{}", h_head("Useful environment variables"));
  help_row("SCSH_RUNTIME", "Force container runtime: docker, podman, or container (Apple).");
  help_row(
    "SCSH_GIT_TRANSPORT",
    "Force git push/fetch transport (1) or bind-mount clone (0). Ignored on macOS Apple Container.",
  );
  help_row(
    runtime::GIT_TRANSPORT_HOST_ENV,
    "Override git-daemon host IP inside the container (default: ip route gateway).",
  );
  help_row("SCSH_KEEP_RUNS=1", "Keep every /tmp/scsh-*-run-* clone (also skips stale sweep).");
  help_row("SCSH_NO_OPENCODE_AUTH=1", "Do not forward opencode credentials into containers.");
  help_row("SCSH_NO_CLAUDE_AUTH=1", "Do not forward Claude credentials into containers.");
  help_row("SCSH_NO_CURSOR_AUTH=1", "Do not forward Cursor credentials into containers.");
  println!();
  println!("{}", h_head("After a run"));
  println!("{}", h_dim("  Read each skill's declared `result` path (usually under tmp/). On failure, scsh"));
  println!("{}", h_dim("  prints the kept run-clone path — inspect tmp/scsh-run.log there for full harness output."));
  println!();
  println!("{}", h_head("See also"));
  help_row("scsh help internals", "Repo sync, auth forwarding, live board, image contents.");
  help_row("scsh help .scsh.yml", "Config schema: harness, invocations, env, commits, timeout.");
  help_row("scsh help cache", "When an identical re-run is served from tmp/.sccache/.");
  println!();
}

/// `scsh help .scsh.yml` — the project config file, in full.
fn print_help_config() {
  println!();
  println!("{} {}", h_head(".scsh.yml"), console::style("\u{2014} the project config file").bold());
  println!("{}", h_dim("The whole file is just your skills; scsh owns the container command. The base"));
  println!(
    "{}",
    h_dim("image is built in (Debian + shared base + per-harness CLI) — no version/project/image header.")
  );
  println!();
  print!(
    "{}",
    r#"  terminal:               # optional: PTY size for harness runs (and their .cast recordings)
    cols: 200             #   default 200 (20..500)
    rows: 50              #   default 50 (10..200)
  skills:                 # one entry per .skills/<name>/ folder

    add:                    #   key must match the skill directory name
      harness: opencode     #     direct run — OR use invocations: for a matrix (below)
                            #     harnesses: opencode | claude | codex | grok | cursor
      model: openai/...     #     optional; the model the harness passes to the tool
      effort: high        #     optional; reasoning effort (codex: minimal..xhigh, grok:
                          #       low..max, cursor: low..high as --model slug suffixes). With
                          #       invocations: a default routes may override; harnesses
                          #       without an effort knob ignore it
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
      autoinstall: false  #     optional; default true. false = authoring-only: `installskills`
                          #       won't copy it into a consumer repo (an `internal-` name does the same)
      invocations:        #     optional matrix — each route expands to `{skill}-{route}`
        opencode-gpt:       #       at run and install time; per-route profile/commits override
          harness: opencode
          model: openai/...
      result: tmp/x.json  #     required; use {name} in the path when `invocations:` is set
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
  println!("{}", h_dim("  Discover them programmatically (runtime-free): `scsh list --json`, or gate a script on"));
  println!("{}", h_dim("  `scsh check-profile <name>` (exit 0 iff that profile exists with at least one skill)."));
  println!();
  println!("{}", h_head("Sharing skills (install sources)"));
  println!("{}", h_dim("  When another repo runs `scsh installskills <this-repo>`, scsh installs every skill in"));
  println!("{}", h_dim("  this manifest EXCEPT those marked `autoinstall: false` or named `internal-*` (both"));
  println!("{}", h_dim("  authoring-only), merging the rest verbatim into that repo's own .scsh.yml."));
  println!("{}", h_dim("  Existing skill keys in the consumer are left untouched — scsh warns on conflicts."));
  println!();
  println!("{}", h_dim("Harness commands (inside the container):"));
  println!("{}", h_dim("  opencode: opencode -m <model> run \"run skill <source>\""));
  println!(
    "{}",
    h_dim(
      "  claude:   claude -p \"Run .skills/<source>/SKILL.md …\" \
(CLAUDE_CODE_OAUTH_TOKEN or ~/.claude/.credentials.json)",
    )
  );
  println!(
    "{}",
    h_dim(
      "  codex:    codex exec -m <model> \"Run .skills/<source>/SKILL.md …\" \
(~/.codex/auth.json or OPENAI_API_KEY) — the native harness for GPT models",
    )
  );
  println!(
    "{}",
    h_dim(
      "  grok:     grok -p \"Run .skills/<source>/SKILL.md …\" -m <model> --effort <level> \
(~/.grok/auth.json or XAI_API_KEY) — the native harness for Grok models",
    )
  );
  println!(
    "{}",
    h_dim(
      "  cursor:   cursor-agent -p --force --trust \"Run .skills/<source>/SKILL.md …\" \
(macOS keychain / auth.json / CURSOR_API_KEY) — the native harness for Cursor models",
    )
  );
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
    r#"  scsh builds the shared base image (`scsh-base:latest`) first — or skips it when the tag
  already matches the embedded Dockerfile fingerprint — then builds the needed per-harness
  images (`scsh-opencode`, `scsh-claude`, `scsh-codex`, `scsh-grok`, `scsh-cursor`) on top IN PARALLEL,
  skipping any whose tag already matches. Each build is version-checked during the build. Then, for
  EVERY selected skill in parallel, it prepares a /tmp run dir (scsh-YYYYMMDD-HHMMSS-utc-run-<invocation> on docker/podman,
  or scsh-<nonce>-run-<invocation> on Apple container — ≤ 64 chars, middle-truncated with .. when
  needed) and runs the skill's harness in its own container. On docker/podman/Linux the host
  git-clones into the run dir and bind-mounts it at /home/agent/repo. On macOS Apple Container
  scsh git-pushes into a bare transport repo and the container clones from a per-run git daemon
  (only run_dir/tmp is bind-mounted — results, logs, forwarded auth). The repo lives UNDER the
  agent's home, not as it, so harness scratch stays out of the tree.

  scsh injects SCSH_RESULT=<result path> into every container so one skill folder can
  serve multiple invocations with different result files.

  Repo sync — push IN, pull OUT (never GitHub from inside the container):
  Host push IN: git clone + bind-mount (docker/podman/Linux), or git push to transport.git +
  container git clone via git:// (Apple Container on macOS). Skills must not git fetch, pull,
  push, or clone remotes inside. After the container exits, scsh on the HOST pulls OUT: (1) the
  result file — always (from bind-mounted tmp/); (2) new commits — only when commits: true AND
  the skill committed — via local git fetch from the run clone or pull.git and cherry-pick.
  scsh never pushes to any remote. Reviewer skills are review-only (no commits).

  Each skill MUST produce its declared `result` file. Missing after the container
  exits -> that skill fails and the whole invocation exits non-zero; otherwise scsh
  copies the result back into your repo, moving any existing file aside to
  <name>.bak.YYYYMMDD-HHMMSS-utc. All skills run regardless, so one run reports
  every skill's outcome.

  Auth: opencode skills copy the host ~/.local/share/opencode/auth.json and
  ~/.config/opencode/opencode.json (plus optional opencode.jsonc) into each run clone,
  then bind-mount from there (needed for custom providers such as Nebius GLM;
  opt out: SCSH_NO_OPENCODE_AUTH=1).
  Claude skills use host CLAUDE_CODE_OAUTH_TOKEN (from `claude setup-token`) and/or
  ~/.claude/.credentials.json, copied into the run dir and bind-mounted into the container
  (opt out: SCSH_NO_CLAUDE_AUTH=1).
  Codex skills copy the host ~/.codex/auth.json and config.toml (from `codex login`) into the
  run clone's gitignored tmp/.codex — the image's CODEX_HOME — and forward OPENAI_API_KEY when
  set; the credentials are scrubbed from the run dir after the container exits
  (opt out: SCSH_NO_CODEX_AUTH=1). Codex is the recommended native harness for GPT models.
  Grok skills work the same way: host ~/.grok/auth.json + config.toml (from `grok login`)
  are copied into tmp/.grok — the image's GROK_HOME — XAI_API_KEY is forwarded when set,
  and credentials are scrubbed after exit (opt out: SCSH_NO_GROK_AUTH=1). Grok is the
  recommended native harness for Grok models.
  Cursor skills copy the host ~/.cursor/cli-config.json and optional mcp.json into tmp/.cursor,
  OAuth tokens from ~/.config/cursor/auth.json or the macOS login keychain into tmp/.config/cursor/auth.json,
  CURSOR_API_KEY is forwarded when set, and credentials are scrubbed after exit (opt out: SCSH_NO_CURSOR_AUTH=1). Cursor is
  the native harness for Cursor Agent models (Composer, etc.).
  Harness runs at full verbosity (OpenCode DEBUG + --print-logs; Claude --verbose --debug;
  Codex RUST_LOG tracing + its final message appended to the log; Grok --debug + its debug
  log appended; Cursor --output-format stream-json);
  every line is teed to tmp/scsh-run.log and the session browser daemon (opt out: SCSH_QUIET=1).
  A transient infra failure (timeout, container/clone error) is retried once on a fresh
  clone (opt out: SCSH_NO_RETRY=1); failures land in `scsh failures` with stable reason codes.
  Every skill outcome is also recorded durably in ~/.scsh/stats.jsonl — route, duration,
  attempts, and the branch workload (commits + LOC over main) — browse with `scsh stats`.
  Unavailable harnesses and opencode models are skipped; a run fails only when every selected skill is skipped.
  Every line of harness output is teed to <run_dir>/tmp/scsh-run.log for inspection.

  The Dockerfile is generated in memory (streamed to the builder's stdin), and your
  repository is modified only by the result copies (into the gitignored tmp/).

  Cleanup: a skill's container is --rm, and its /tmp clone is host-side scratch. After a
  SUCCESSFUL skill scsh removes that clone; a FAILED skill's clone is kept for inspection
  (its path is printed). Stale clones from past runs (>24h old) are swept at the next run's
  start. Keep every clone with SCSH_KEEP_RUNS=1 (also skips the sweep).

  The live board: on a terminal the build and every skill are drawn as collapsible rows,
  inline in the normal buffer (no alternate screen, so your scrollback keeps working). Each row
  carries a [0]..[9], [A]..[Z] label on the left: press that digit or letter to expand/collapse the row
  (scsh turns on the terminal's keyboard-enhancement protocol so Ctrl+digit works too; without it, the
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
                     perl, gawk, node
    harness images   scsh-opencode (+ opencode-ai), scsh-claude (+ @anthropic-ai/claude-code)
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

#[cfg(test)]
mod tests {
  use super::*;
  use std::ffi::OsString;

  #[test]
  fn opencode_auth_path_prefers_xdg_then_home() {
    let xdg = OsString::from("/data");
    let home = OsString::from("/home/u");
    assert_eq!(runtime::opencode_auth_in(Some(&xdg), Some(&home)), Some(PathBuf::from("/data/opencode/auth.json")));
    // No XDG → HOME/.local/share.
    assert_eq!(
      runtime::opencode_auth_in(None, Some(&home)),
      Some(PathBuf::from("/home/u/.local/share/opencode/auth.json"))
    );
    // Empty XDG falls back to HOME too.
    let empty = OsString::from("");
    assert_eq!(
      runtime::opencode_auth_in(Some(&empty), Some(&home)),
      Some(PathBuf::from("/home/u/.local/share/opencode/auth.json"))
    );
    // Nothing to go on → None.
    assert_eq!(runtime::opencode_auth_in(None, None), None);
  }

  #[test]
  fn sweep_removes_only_matching_stale_run_dirs() {
    let base = std::env::temp_dir().join(format!("scsh-sweeptest-{}-{}", std::process::id(), now_secs()));
    std::fs::create_dir_all(&base).unwrap();
    // A matching run-dir, a non-matching scsh dir, an unrelated dir, and a matching *file*.
    let run = base.join("scsh-20231114-221320-utc-run-add");
    let run_apple = base.join("scsh-abcdef-run-add");
    let install = base.join("scsh-installskills-1-2");
    let other = base.join("some-other-dir");
    let run_file = base.join("scsh-19700101-000000-utc-run-x"); // a file, not a dir
    std::fs::create_dir(&run).unwrap();
    std::fs::create_dir(&run_apple).unwrap();
    std::fs::create_dir(&install).unwrap();
    std::fs::create_dir(&other).unwrap();
    std::fs::write(&run_file, b"").unwrap();
    let now = now_secs();

    // A threshold beyond any real age sweeps nothing (an in-progress run is safe).
    assert_eq!(sweep_stale_run_dirs_in(&base, now, u64::MAX), 0);
    assert!(run.is_dir());

    // A zero threshold makes every just-created entry "stale" — but only matching
    // DIRECTORIES are removed; a non run-dir, an unrelated dir, and a matching file are left.
    assert_eq!(sweep_stale_run_dirs_in(&base, now, 0), 2);
    assert!(!run.exists(), "the UTC-stamped run dir is removed");
    assert!(!run_apple.exists(), "the Apple-container run dir is removed");
    assert!(install.is_dir(), "a non run-dir scsh dir is left alone");
    assert!(other.is_dir(), "an unrelated dir is left alone");
    assert!(run_file.is_file(), "a matching *file* (not a dir) is left alone");

    std::fs::remove_dir_all(&base).unwrap();
  }

  #[test]
  fn run_positional_args_are_profiles() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    // A bare positional after `run` is a profile — `run foo` == `run --profile foo`.
    let c = cli(&["run", "foo"]).unwrap();
    assert!(matches!(c.mode, Mode::Run));
    assert_eq!(c.profile.as_deref(), Some("foo"));
    // Several positionals == a comma list — `run foo bar` == `run --profile foo,bar`.
    assert_eq!(cli(&["run", "foo", "bar"]).unwrap().profile.as_deref(), Some("foo,bar"));
    assert_eq!(cli(&["run", "--profile", "foo,bar"]).unwrap().profile.as_deref(), Some("foo,bar"));
    // `--profile` and positionals combine.
    assert_eq!(cli(&["run", "--profile", "foo", "bar"]).unwrap().profile.as_deref(), Some("foo,bar"));
    // No profile at all → None (the reserved `default` profile runs).
    assert_eq!(cli(&["run"]).unwrap().profile, None);
    // Positional profiles are `run`-only, and never swallow flags or other commands.
    assert!(cli(&["foo"]).is_err(), "a bare token without `run` is an unknown command");
    assert!(cli(&["list", "foo"]).is_err(), "profiles don't apply to `list`");
    assert!(cli(&["run", "--nope"]).is_err(), "an unknown flag after `run` is not a profile");
  }

  #[test]
  fn failures_command_parses_filters_and_stats() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["failures"]).unwrap();
    assert!(matches!(c.mode, Mode::Failures));
    assert!(!c.failures.stats);
    let c = cli(&["failures", "--session", "abc123", "--skill", "add", "--reason", "clone_failed"]).unwrap();
    assert_eq!(c.failures.session.as_deref(), Some("abc123"));
    assert_eq!(c.failures.skill.as_deref(), Some("add"));
    assert_eq!(c.failures.reason.as_deref(), Some("clone_failed"));
    let c = cli(&["failures", "--stats", "--last", "0"]).unwrap();
    assert!(c.failures.stats);
    assert_eq!(c.failures.last, Some(0));
    assert!(cli(&["failures", "--last", "many"]).is_err(), "--last needs a number");
    assert!(cli(&["run", "--stats"]).is_err(), "failure filters don't apply to run");
    assert!(cli(&["list", "--session", "abc"]).is_err(), "failure filters don't apply to list");
  }

  #[test]
  fn stats_command_parses_filters() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["stats"]).unwrap();
    assert!(matches!(c.mode, Mode::Stats));
    let c = cli(&["stats", "--skill", "conventions-reviewer", "--harness", "codex", "--model", "gpt-5.5"]).unwrap();
    assert_eq!(c.failures.skill.as_deref(), Some("conventions-reviewer"));
    assert_eq!(c.failures.harness.as_deref(), Some("codex"));
    assert_eq!(c.failures.model.as_deref(), Some("gpt-5.5"));
    let c = cli(&["stats", "--profile", "code-review", "--raw", "--last", "10"]).unwrap();
    assert_eq!(c.profile.as_deref(), Some("code-review"));
    assert!(c.failures.raw);
    assert_eq!(c.failures.last, Some(10));
    assert!(cli(&["run", "--raw"]).is_err(), "--raw only applies to stats");
    assert!(cli(&["failures", "--harness", "codex"]).is_err(), "--harness only applies to stats");
    assert!(cli(&["stats", "--reason", "clone_failed"]).is_err(), "--reason only applies to failures");
  }

  #[test]
  fn prune_command_parses_now_flag() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["prune"]).unwrap();
    assert!(matches!(c.mode, Mode::Prune));
    assert!(!c.prune_now);
    assert!(cli(&["prune", "--now"]).unwrap().prune_now);
    assert!(cli(&["run", "--now"]).is_err(), "--now only applies to prune");
  }

  #[test]
  fn opencode_config_path_prefers_xdg_then_home() {
    let xdg = OsString::from("/cfg");
    let home = OsString::from("/home/u");
    assert_eq!(runtime::opencode_config_dir(Some(&xdg), Some(&home)), Some(PathBuf::from("/cfg/opencode")));
    assert_eq!(runtime::opencode_config_dir(None, Some(&home)), Some(PathBuf::from("/home/u/.config/opencode")));
  }

  #[test]
  fn prepare_opencode_mount_dirs_creates_xdg_parent() {
    let run = std::env::temp_dir().join(format!("scsh-auth-{}-{}", std::process::id(), now_secs()));
    std::fs::create_dir_all(&run).unwrap();
    prepare_opencode_mount_dirs(&run);
    assert!(run.join("tmp/.xdg-data/opencode").is_dir());
    let _ = std::fs::remove_dir_all(&run);
  }

  // --- commit integration ---------------------------------------------------
  // These exercise integrate_commits against real (synthetic) git repos — no
  // container needed — so the rebase / fallback-branch / run-twice behavior is
  // pinned down in CI. (The full container round-trip is shown in DEMO.md.)

  use std::sync::atomic::{AtomicUsize, Ordering};
  static MT: AtomicUsize = AtomicUsize::new(0);

  fn mt_dir(tag: &str) -> PathBuf {
    let n = MT.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("scsh-mt-{tag}-{}-{}-{n}", std::process::id(), now_secs()));
    std::fs::create_dir_all(&d).unwrap();
    d
  }

  fn g(dir: &Path, args: &[&str]) {
    assert!(git_status_ok(dir, args), "git {args:?} should succeed in {}", dir.display());
  }

  fn head(dir: &Path) -> String {
    git_capture(dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
  }

  /// A fresh repo with one `base` commit and a local identity.
  fn repo(tag: &str) -> PathBuf {
    let d = mt_dir(tag);
    g(&d, &["init", "-q", "."]);
    g(&d, &["config", "user.email", "t@e.st"]);
    g(&d, &["config", "user.name", "tester"]);
    std::fs::write(d.join("README"), "base\n").unwrap();
    g(&d, &["add", "-A"]);
    g(&d, &["commit", "-qm", "base"]);
    d
  }

  /// Clone `src` and commit a change in the clone (mimicking a commit-enabled skill).
  fn clone_and_commit(src: &Path, tag: &str, file: &str, contents: &str, msg: &str) -> PathBuf {
    let d = mt_dir(tag);
    assert!(
      Command::new("git")
        .args(["clone", "-q", &src.to_string_lossy(), &d.to_string_lossy()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false),
      "clone should succeed"
    );
    set_clone_identity(&d);
    std::fs::write(d.join(file), contents).unwrap();
    g(&d, &["add", "-A"]);
    g(&d, &["commit", "-qm", msg]);
    d
  }

  #[test]
  fn incoming_branch_name_is_distinct_and_descriptive() {
    let n = incoming_branch_name("add", "20231114-221320", "abcdef1234567");
    assert_eq!(n, "scsh/incoming/add-20231114-221320-utc-abcdef1");
    // A messy skill name is sanitized into a valid ref component.
    assert!(incoming_branch_name("My Skill!", "S", "deadbeef").starts_with("scsh/incoming/my-skill-S-utc-"));
  }

  #[test]
  fn integrate_rebases_clean_commits_onto_the_branch() {
    let caller = repo("clean-caller");
    let base = head(&caller);
    let clone = clone_and_commit(&caller, "clean-clone", "foo.txt", "hi\n", "add foo");

    let outcome = integrate_commits(&caller, &clone, &base, "add", "STAMP").unwrap();
    assert!(matches!(outcome, Some(Integration::Applied { count: 1 })), "expected 1 applied commit");
    // The file is now committed on the caller's branch, the tree is clean, and HEAD
    // advanced by exactly one commit.
    assert_eq!(std::fs::read_to_string(caller.join("foo.txt")).unwrap(), "hi\n");
    assert_eq!(git_capture(&caller, &["status", "--porcelain"]).unwrap().trim(), "");
    assert_eq!(git_capture(&caller, &["rev-list", "--count", &format!("{base}..HEAD")]).unwrap().trim(), "1");
    // The brought-in commit keeps the deliberately recognizable bot author (the
    // "not-for-pushing" tripwire); the committer is the caller.
    assert_eq!(git_capture(&caller, &["log", "-1", "--format=%ae"]).unwrap().trim(), SCSH_COMMIT_EMAIL);
    assert_eq!(git_capture(&caller, &["log", "-1", "--format=%an"]).unwrap().trim(), SCSH_COMMIT_NAME);
  }

  #[test]
  fn integrate_rebases_second_skill_onto_advanced_head() {
    // Two skills both branched from the same base; the second must rebase onto the
    // HEAD the first advanced to (not fast-forward), ending with BOTH files.
    let caller = repo("two-caller");
    let base = head(&caller);
    let c1 = clone_and_commit(&caller, "two-c1", "a.txt", "A\n", "add a");
    let c2 = clone_and_commit(&caller, "two-c2", "b.txt", "B\n", "add b");

    assert!(matches!(integrate_commits(&caller, &c1, &base, "s1", "S").unwrap(), Some(Integration::Applied { .. })));
    assert!(matches!(integrate_commits(&caller, &c2, &base, "s2", "S").unwrap(), Some(Integration::Applied { .. })));
    assert!(caller.join("a.txt").is_file() && caller.join("b.txt").is_file(), "both skills' files land");
    assert_eq!(git_capture(&caller, &["rev-list", "--count", &format!("{base}..HEAD")]).unwrap().trim(), "2");
  }

  #[test]
  fn integrate_saves_conflicting_commits_to_a_branch() {
    let caller = repo("conf-caller");
    let base = head(&caller);
    // Both skills are cloned up front from the SAME base (as scsh does), and both add
    // the same file with different content.
    let c1 = clone_and_commit(&caller, "conf-c1", "shared.txt", "one\n", "shared one");
    let c2 = clone_and_commit(&caller, "conf-c2", "shared.txt", "two\n", "shared two");
    // The first applies cleanly; cherry-picking the second onto the now-advanced caller
    // (which already has shared.txt="one") is an add/add conflict.
    integrate_commits(&caller, &c1, &base, "s1", "S").unwrap();
    let outcome = integrate_commits(&caller, &c2, &base, "s2", "S").unwrap();
    let branch = match outcome {
      Some(Integration::Saved { branch, count: 1 }) => branch,
      other => panic!("expected the conflicting commit to be saved to a branch, got {:?}", other.is_some()),
    };
    // The caller's branch is untouched (still "one"); the fallback branch exists and
    // carries the skill's commit.
    assert_eq!(std::fs::read_to_string(caller.join("shared.txt")).unwrap(), "one\n");
    assert_eq!(git_capture(&caller, &["status", "--porcelain"]).unwrap().trim(), "", "no half-applied cherry-pick");
    assert!(branch.starts_with("scsh/incoming/s2-"));
    assert_eq!(git_capture(&caller, &["cat-file", "-t", &branch]).unwrap().trim(), "commit");
  }

  #[test]
  fn integrate_is_a_noop_when_the_skill_added_no_commits() {
    let caller = repo("noop-caller");
    let base = head(&caller);
    let d = mt_dir("noop-clone");
    assert!(Command::new("git")
      .args(["clone", "-q", &caller.to_string_lossy(), &d.to_string_lossy()])
      .status()
      .unwrap()
      .success());
    // No commit made in the clone → nothing to bring back.
    assert!(integrate_commits(&caller, &d, &base, "add", "S").unwrap().is_none());
  }

  #[test]
  fn commits_are_a_side_effect_run_twice_adds_twice() {
    // Models a skill that appends a line and commits, run on two consecutive
    // invocations (each captures its own base = the current HEAD). The result is two
    // commits and a two-line file — adding a commit is a side effect, not deduped.
    let caller = repo("twice-caller");
    let base1 = head(&caller);
    let r1 = clone_and_commit(&caller, "twice-r1", "log.txt", "x\n", "log x");
    integrate_commits(&caller, &r1, &base1, "add", "S").unwrap();

    let base2 = head(&caller); // the next run's base is the now-advanced HEAD
    let r2 = clone_and_commit(&caller, "twice-r2", "log.txt", "x\nx\n", "log x again");
    integrate_commits(&caller, &r2, &base2, "add", "S").unwrap();

    assert_eq!(std::fs::read_to_string(caller.join("log.txt")).unwrap(), "x\nx\n");
    assert_eq!(git_capture(&caller, &["rev-list", "--count", &format!("{base1}..HEAD")]).unwrap().trim(), "2");
  }

  // --- result cache ---------------------------------------------------------

  fn mk_inv(name: &str) -> config::ResolvedInvocation {
    config::ResolvedInvocation {
      name: name.into(),
      skill_source: name.into(),
      harness: config::Harness::Opencode,
      model: None,
      effort: None,
      timeout: None,
      env: Vec::new(),
      profile: None,
      commits: false,
      result: "tmp/r.json".into(),
      terminal: config::Terminal::default(),
    }
  }

  #[test]
  fn cache_key_is_deterministic_and_sensitive() {
    let caller = repo("ck");
    std::fs::create_dir_all(caller.join(".skills/add")).unwrap();
    std::fs::write(caller.join(".skills/add/SKILL.md"), "name: add\nbody\n").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "skill"]);
    let s = mk_inv("add");
    let env = vec![("A".to_string(), "2".to_string()), ("B".to_string(), "3".to_string())];

    let k1 = cache_key(&caller, &s, &env).unwrap();
    assert_eq!(k1.len(), 64, "key is a sha256 hex digest");
    // Same inputs => same key; env order doesn't matter (it's sorted).
    assert_eq!(cache_key(&caller, &s, &env).unwrap(), k1);
    let env_rev = vec![("B".to_string(), "3".to_string()), ("A".to_string(), "2".to_string())];
    assert_eq!(cache_key(&caller, &s, &env_rev).unwrap(), k1);
    // Different env => different key.
    let env2 = vec![("A".to_string(), "9".to_string()), ("B".to_string(), "3".to_string())];
    assert_ne!(cache_key(&caller, &s, &env2).unwrap(), k1);

    // A committed change to repo content => different key (the HEAD tree changed).
    std::fs::write(caller.join("other.txt"), "x").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "change"]);
    assert_ne!(cache_key(&caller, &s, &env).unwrap(), k1);

    // Editing the skill body => different key too.
    let before_skill_edit = cache_key(&caller, &s, &env).unwrap();
    std::fs::write(caller.join(".skills/add/SKILL.md"), "name: add\nNEW body\n").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "edit skill"]);
    assert_ne!(cache_key(&caller, &s, &env).unwrap(), before_skill_edit);
  }

  #[test]
  fn cache_store_lookup_and_restore_roundtrip() {
    let caller = repo("cs");
    let key = "deadbeef";
    assert!(cache_lookup(&caller, key).is_none(), "empty cache misses");

    let result = r#"{"result": "2 + 3 = 5"}"#;
    cache_store(&caller, key, result, None);
    // Stored under the repo's gitignored tmp/.sccache/<key>.json, and reads back.
    assert!(caller.join("tmp/.sccache").join(format!("{key}.json")).is_file());
    let entry = cache_lookup(&caller, key).expect("hit");
    assert_eq!(entry.result, result);
    assert!(entry.commits.is_none(), "no commits journaled for a non-committing skill");

    // Restoring writes the result file (creating tmp/), exactly as a real run would have.
    restore_cached_result(&caller, "tmp/add_result.json", result).unwrap();
    assert_eq!(std::fs::read_to_string(caller.join("tmp/add_result.json")).unwrap(), result);
    // And the human message is recoverable from the restored content.
    assert_eq!(json::message(result).as_deref(), Some("2 + 3 = 5"));

    // A commit-enabled skill journals its commits (a patch mbox); they round-trip and a
    // multi-line patch with quotes survives the JSON quoting.
    let patch = r#"From abc Mon Sep 17 00:00:00 2001
Subject: [PATCH] add: 2 + 3 = 5

"diff" body
"#;
    cache_store(&caller, "withcommit", result, Some(patch));
    let e2 = cache_lookup(&caller, "withcommit").expect("hit");
    assert_eq!(e2.result, result);
    assert_eq!(e2.commits.as_deref(), Some(patch));
  }

  #[test]
  fn cached_commits_are_replayed() {
    let caller = repo("replay");
    let base = git_capture(&caller, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
    // Make a commit, capture it as a patch (what cache_store journals), then revert to base.
    std::fs::write(caller.join("note.txt"), "hi\n").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "add note"]);
    let patch = commit_patch(&caller, &base).expect("a commit patch");
    g(&caller, &["reset", "--hard", &base]);
    assert!(!caller.join("note.txt").exists(), "reverted to base");
    // Replaying the journaled patch (what a cache HIT does) brings the commit back.
    let res = apply_cached_commits(&caller, &patch, "demo", "20260101-000000");
    assert!(matches!(res, Ok(Some(Integration::Applied { count: 1 }))), "expected Applied{{count:1}}");
    assert!(caller.join("note.txt").exists(), "replayed file is present");
    assert_eq!(git_capture(&caller, &["log", "-1", "--format=%s"]).unwrap().trim(), "add note");
    assert_ne!(git_capture(&caller, &["rev-parse", "HEAD"]).unwrap().trim(), base, "HEAD advanced past base");
  }
}
