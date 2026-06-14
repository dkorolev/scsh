//! scsh — Scoped Skills Helper.
//!
//! Preflight a git repository (git → repo → `.scsh.yml` present → schema-valid →
//! a container runtime), then build one in-memory image and run the project's
//! scoped skills — all of them, in parallel, each in its own ephemeral container
//! under its configured harness.

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
  match cli.mode {
    Mode::Help(topic) => {
      print_help(topic);
      0
    }
    Mode::Version => {
      println!("scsh {}", version_id());
      0
    }
  }
}

#[derive(Clone, Copy)]
enum Mode {
  Help(HelpTopic),
  Version,
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

/// A parsed command line. For now it carries just the command; options arrive with
/// the commands that need them.
struct Cli {
  mode: Mode,
}

/// Parse cargo-style subcommands. The default (no command) is `help`, so a bare
/// `scsh` is safe and self-explanatory.
fn parse_cli(args: &[String]) -> Result<Cli, String> {
  let mut mode: Option<Mode> = None;
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
          Some(other) if !other.starts_with('-') => {
            return Err(format!("unknown help topic '{other}' (topics: .scsh.yml, internals, cache)"));
          }
          _ => HelpTopic::Overview,
        };
        Some(Mode::Help(topic))
      }
      "version" | "-V" | "--version" => Some(Mode::Version),
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
  Ok(Cli { mode: mode.unwrap_or(Mode::Help(HelpTopic::Overview)) })
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
