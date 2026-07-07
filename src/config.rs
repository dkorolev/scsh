//! `.scsh.yml` parsing and schema validation.
//!
//! scsh keeps its own logic dependency-free, so rather than pull in a full YAML
//! library it carries a deliberately small parser that understands just the
//! subset of YAML the config needs: comments, `key: value` pairs, single-quoted
//! / double-quoted scalars, and nested mappings (the `skills:` block nests two
//! levels — skill name, then its fields). Anything outside that subset is
//! reported as an error rather than silently mis-parsed.

use std::collections::BTreeMap;

/// A parsed-and-validated `.scsh.yml`. For v1.0 the file is just its skills — there is no
/// `version`/`project`/`image` boilerplate; the base image (a glibc Debian dev/CLI toolchain)
/// is fixed and owned by the generated [`crate::runtime::dockerfile`] (`src/Dockerfile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
  /// One or more scoped skills, in file order. scsh runs them all in parallel.
  pub skills: Vec<Skill>,
  /// PTY geometry for harness runs; every harness runs (and is asciinema-recorded)
  /// inside a pseudo-terminal of this size. Optional top-level `terminal:` block.
  pub terminal: Terminal,
}

/// Terminal size for the pseudo-terminal each harness runs in (`terminal: {cols, rows}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal {
  pub cols: u16,
  pub rows: u16,
}

pub const DEFAULT_TERMINAL_COLS: u16 = 200;
pub const DEFAULT_TERMINAL_ROWS: u16 = 50;

impl Default for Terminal {
  fn default() -> Self {
    Terminal { cols: DEFAULT_TERMINAL_COLS, rows: DEFAULT_TERMINAL_ROWS }
  }
}

/// One manifest row in `.scsh.yml`. The key must match the `.skills/<name>/` folder.
/// Either declare direct run fields (`harness`, optional `model`, …) for a single
/// invocation, or an `invocations:` matrix — each route expands to `{name}-{route}` at
/// run time and on install (same schema in source and consumer repos).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
  /// The `.scsh.yml` key — must match `.skills/<name>/`.
  pub name: String,
  /// Direct-run harness. Required when `invocations` is empty; must be omitted when
  /// `invocations` is set.
  pub harness: Option<Harness>,
  pub model: Option<String>,
  /// Reasoning effort for harnesses with an effort knob (codex, grok, cursor). With
  /// `invocations:` it is the default each route may override; routes whose harness
  /// has no effort knob simply ignore an inherited value.
  pub effort: Option<String>,
  pub timeout: Option<u64>,
  pub env: Vec<EnvVar>,
  /// Default profile for direct runs, or for matrix routes that omit their own `profile:`.
  pub profile: Option<String>,
  pub commits: bool,
  /// Consulted only by `installskills`/`updateskills`.
  pub autoinstall: bool,
  /// Matrix routes. Each expands to a [`ResolvedInvocation`] named `{name}-{route}`.
  pub invocations: Vec<InvocationRoute>,
  /// Output path. With `invocations`, must contain `{name}` (substituted per route).
  pub result: String,
}

/// One row under a skill's `invocations:` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationRoute {
  pub name: String,
  pub harness: Harness,
  pub model: Option<String>,
  /// When set, overrides the skill-level `effort:` for this route only.
  pub effort: Option<String>,
  /// When set, overrides the skill-level `profile:` for this route only.
  pub profile: Option<String>,
  /// When set, overrides the skill-level `commits:` for this route only.
  pub commits: Option<bool>,
}

/// A concrete run invocation after expanding matrix skills — what `scsh run` executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInvocation {
  pub name: String,
  pub skill_source: String,
  pub harness: Harness,
  pub model: Option<String>,
  /// Reasoning effort; only ever set for harnesses with an effort knob.
  pub effort: Option<String>,
  pub timeout: Option<u64>,
  pub env: Vec<EnvVar>,
  pub profile: Option<String>,
  pub commits: bool,
  pub result: String,
  /// PTY size the harness runs (and is recorded) in — the config's top-level `terminal:`.
  pub terminal: Terminal,
  /// For a harness-definition run (`scsh run --def`), the `SKILL.md` body to materialize
  /// into the run clone as `.skills/<skill_source>/SKILL.md` (the repo is clean, so the body
  /// cannot live in the working tree). `None` for a normal `.scsh.yml` skill, whose body is
  /// the committed `.skills/<name>/SKILL.md`.
  pub body: Option<String>,
}

/// Expand every manifest skill into the invocation(s) `scsh run` would execute, in file order.
pub fn expand_invocations(cfg: &Config) -> Vec<ResolvedInvocation> {
  cfg.skills.iter().flat_map(|s| expand_skill(s, cfg.terminal)).collect()
}

fn expand_skill(skill: &Skill, terminal: Terminal) -> Vec<ResolvedInvocation> {
  // An inherited skill-level effort applies only where the harness has an effort knob,
  // so one `effort:` can sit atop a mixed matrix (codex + claude) without erroring.
  let effort_for = |harness: Harness, route_effort: Option<&String>| -> Option<String> {
    let effort = route_effort.or(skill.effort.as_ref())?;
    harness.supports_effort().then(|| effort.clone())
  };
  if skill.invocations.is_empty() {
    let harness = skill.harness.expect("validated skills always have harness or invocations");
    return vec![ResolvedInvocation {
      name: skill.name.clone(),
      skill_source: skill.name.clone(),
      harness,
      model: skill.model.clone(),
      effort: effort_for(harness, None),
      timeout: skill.timeout,
      env: skill.env.clone(),
      profile: skill.profile.clone(),
      commits: skill.commits,
      result: skill.result.clone(),
      terminal,
      body: None,
    }];
  }
  skill
    .invocations
    .iter()
    .map(|route| ResolvedInvocation {
      name: format!("{}-{}", skill.name, route.name),
      skill_source: skill.name.clone(),
      harness: route.harness,
      model: route.model.clone(),
      effort: effort_for(route.harness, route.effort.as_ref()),
      timeout: skill.timeout,
      env: skill.env.clone(),
      profile: route.profile.clone().or_else(|| skill.profile.clone()),
      commits: route.commits.unwrap_or(skill.commits),
      result: skill.result.replace("{name}", &route.name),
      terminal,
      body: None,
    })
    .collect()
}

/// One `env:` entry: a variable to set inside the container, and the rule for
/// where its value comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvVar {
  /// The variable name set inside the container.
  pub key: String,
  pub rule: EnvRule,
}

/// How a forwarded variable's value is resolved, mirroring shell parameter
/// expansion (the value side of an `env:` entry). scsh resolves the value against
/// the host environment and sets it inside the container — or refuses the skill
/// when a required variable is unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvRule {
  /// `${SRC}` / `$SRC` (and `${SRC:?message}`) — the variable is **required**.
  /// scsh forwards the host's value, or refuses the skill (printing `message`) when
  /// it is unset, before any work is done. A bare `${SRC}` / `$SRC` gets a default
  /// message pointing at the `${SRC:-}` form.
  Require { src: String, message: String },
  /// `${SRC:-default}` — forward the host's `src` when set, otherwise inject
  /// `default` (so `${SRC:-}` injects an empty value). scsh resolves and sets it.
  Default { src: String, default: String },
  /// A literal value (no `$…`), always set verbatim.
  Constant(String),
}

/// The built-in harness that runs a skill inside the container: `opencode`, `claude`
/// (Claude Code), `codex` (OpenAI Codex CLI — the native harness for GPT models),
/// `grok` (xAI Grok CLI — the native harness for Grok models), or `cursor` (Cursor
/// Agent CLI — `cursor agent` headless).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Harness {
  Opencode,
  Claude,
  Codex,
  Grok,
  Cursor,
}

impl Harness {
  /// Every harness, in `.scsh.yml` declaration order — one scsh image each.
  pub const ALL: [Harness; 5] = [Harness::Opencode, Harness::Claude, Harness::Codex, Harness::Grok, Harness::Cursor];

  /// Parse a `harness:` value; `None` for an unknown harness.
  pub fn parse(s: &str) -> Option<Harness> {
    match s {
      "opencode" => Some(Harness::Opencode),
      "claude" => Some(Harness::Claude),
      "codex" => Some(Harness::Codex),
      "grok" => Some(Harness::Grok),
      "cursor" => Some(Harness::Cursor),
      _ => None,
    }
  }

  /// The canonical name, as written in `.scsh.yml`.
  pub fn as_str(self) -> &'static str {
    match self {
      Harness::Opencode => "opencode",
      Harness::Claude => "claude",
      Harness::Codex => "codex",
      Harness::Grok => "grok",
      Harness::Cursor => "cursor",
    }
  }

  /// Every known harness name, for error messages.
  pub fn known() -> &'static [&'static str] {
    &["opencode", "claude", "codex", "grok", "cursor"]
  }

  /// Whether this harness runs as an interactive TUI recorded under tmux + asciinema (rather
  /// than a headless print/exec mode). For a TUI harness a missing result file can be a transient
  /// external interruption (a stray signal / teardown killing the pane) worth one retry, not only
  /// a deterministic skill bug.
  pub fn is_tui(self) -> bool {
    matches!(self, Harness::Claude | Harness::Codex | Harness::Cursor)
  }

  /// Whether this harness has a reasoning-effort knob (`effort:` in `.scsh.yml`):
  /// grok passes `--effort`, codex passes `-c model_reasoning_effort=`, cursor appends
  /// a hyphen suffix on `--model` (e.g. `claude-opus-4-8-low`, `composer-2.5-fast`).
  pub fn supports_effort(self) -> bool {
    !self.effort_levels().is_empty()
  }

  /// The effort levels this harness's CLI accepts (empty = no effort knob).
  pub fn effort_levels(self) -> &'static [&'static str] {
    match self {
      Harness::Codex => &["minimal", "low", "medium", "high", "xhigh"],
      Harness::Grok => &["low", "medium", "high", "xhigh", "max"],
      Harness::Cursor => &["low", "medium", "high"],
      Harness::Opencode | Harness::Claude => &[],
    }
  }
}

/// A node in the tiny YAML tree: either a scalar string or a mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
  Scalar(String),
  Map(Vec<(String, Node)>),
}

/// A single significant (non-blank, non-comment) input line.
struct Line {
  indent: usize,
  key: String,
  inline: Option<String>,
  lineno: usize,
}

/// The starter config written by `scsh --init-demo-project`.
pub fn demo_yaml() -> &'static str {
  include_str!("demo.scsh.yml")
}

/// scsh's own skills, embedded at build time, that `scsh installskills` /
/// `scsh updateskills` install into a user's repo (as `(repo-relative path, contents)`
/// pairs). Just the demo/self-test skill — so a no-URL `installskills` gives you a way to
/// exercise scsh end to end (real skills come from a repo URL; see `install_skills`). These
/// are distinct from the demo-only example skills in [`demo_skills`].
pub fn bundled_skills() -> [(&'static str, &'static str); 1] {
  [(
    ".skills/scsh-harness-demo-and-selftest/SKILL.md",
    include_str!("../.skills/scsh-harness-demo-and-selftest/SKILL.md"),
  )]
}

/// The example skills `scsh --init-demo-project` scaffolds into the repo, as
/// `(repo-relative path, file contents)` pairs. The bodies are scsh's own
/// canonical example skills, embedded at build time so the scaffold can never
/// drift from them. Each `SKILL.md` is self-describing — a host (opencode, or a
/// human) reads it and performs the skill — so they are runnable as soon as they
/// land in a repo.
/// Each entry is `(repo-relative path, contents, executable)`. The `scripts/*.py` files
/// are scaffolded with the executable bit set so the harness can run them directly.
pub fn demo_skills() -> [(&'static str, &'static str, bool); 4] {
  [
    (".skills/add/SKILL.md", include_str!("../.skills/add/SKILL.md"), false),
    (".skills/add/scripts/add.py", include_str!("../.skills/add/scripts/add.py"), true),
    (".skills/multiply/SKILL.md", include_str!("../.skills/multiply/SKILL.md"), false),
    (".skills/multiply/scripts/multiply.py", include_str!("../.skills/multiply/scripts/multiply.py"), true),
  ]
}

/// Extract a skill's raw `.scsh.yml` block — the `  <name>:` header and indented fields —
/// with `autoinstall:` removed (source-only). Used by `installskills` to merge the block
/// verbatim into the consumer's `.scsh.yml`.
pub fn extract_skill_block(yaml: &str, name: &str) -> Option<String> {
  let want = format!("{name}:");
  let lines: Vec<&str> = yaml.lines().collect();
  let start =
    lines.iter().position(|l| l.starts_with("  ") && !l[2..].starts_with(' ') && l[2..].trim_end() == want)?;
  let mut out = String::new();
  out.push_str(lines[start]);
  out.push('\n');
  for &l in &lines[start + 1..] {
    if !l.trim().is_empty() {
      let indent = l.len() - l.trim_start().len();
      if indent < 4 {
        break;
      }
      if indent == 4 && l.trim_start().starts_with("autoinstall:") {
        continue;
      }
    }
    out.push_str(l);
    out.push('\n');
  }
  while out.ends_with("\n\n") {
    out.pop();
  }
  Some(out)
}

/// Parse and validate config source. Returns every problem found, not just the
/// first, so a single run can point at all the things that need fixing.
pub fn validate(src: &str) -> Result<Config, Vec<String>> {
  let entries = match parse_yaml(src) {
    Ok(e) => e,
    Err(e) => return Err(vec![format!("invalid YAML: {e}")]),
  };

  let mut errors = Vec::new();

  // Index the top-level keys, flagging duplicates. For v1.0 the only key is `skills`.
  let mut top: BTreeMap<&str, &Node> = BTreeMap::new();
  for (k, v) in &entries {
    if top.insert(k.as_str(), v).is_some() {
      errors.push(format!("duplicate top-level key '{k}'"));
    }
  }
  const KNOWN: &[&str] = &["skills", "terminal"];
  for (k, _) in &entries {
    if !KNOWN.contains(&k.as_str()) {
      errors.push(format!("unknown top-level key '{k}' (allowed: skills, terminal)"));
    }
  }

  let terminal = validate_terminal(top.get("terminal").copied(), &mut errors);

  // skills: required mapping of one or more named skills.
  let mut skills = Vec::new();
  match top.get("skills").copied() {
    None => errors.push("missing required key 'skills'".into()),
    Some(Node::Scalar(_)) => errors.push("'skills' must be a mapping of named skills".into()),
    Some(Node::Map(m)) if m.is_empty() => errors.push("'skills' must define at least one skill".into()),
    Some(Node::Map(m)) => {
      let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
      for (name, node) in m {
        if name.trim().is_empty() {
          errors.push("a skill name must not be empty".into());
          continue;
        }
        if seen.insert(name.as_str(), ()).is_some() {
          errors.push(format!("duplicate skill '{name}'"));
        }
        match node {
          Node::Scalar(_) => {
            errors.push(format!("skill '{name}' must be a mapping with 'harness' and 'result'"));
          }
          Node::Map(fields) => {
            if let Some(skill) = validate_skill(name, fields, &mut errors) {
              skills.push(skill);
            }
          }
        }
      }
    }
  }

  if errors.is_empty() {
    let cfg = Config { skills: skills.clone(), terminal };
    let mut by_result: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for inv in expand_invocations(&cfg) {
      by_result.entry(inv.result.clone()).or_default().push(inv.name.clone());
    }
    for (path, names) in by_result {
      if names.len() > 1 {
        errors.push(format!("duplicate result path '{path}' shared by invocations: {}", names.join(", ")));
      }
    }
  }

  if errors.is_empty() {
    Ok(Config { skills, terminal })
  } else {
    Err(errors)
  }
}

/// Validate the optional top-level `terminal:` block; absent keys keep the 200x50 default.
fn validate_terminal(node: Option<&Node>, errors: &mut Vec<String>) -> Terminal {
  let mut term = Terminal::default();
  let Some(node) = node else { return term };
  let fields = match node {
    Node::Scalar(_) => {
      errors.push("'terminal' must be a mapping with 'cols' and/or 'rows'".into());
      return term;
    }
    Node::Map(m) => m,
  };
  let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
  for (k, v) in fields {
    if seen.insert(k.as_str(), ()).is_some() {
      errors.push(format!("duplicate key 'terminal.{k}'"));
      continue;
    }
    let (target, min, max): (&mut u16, u16, u16) = match k.as_str() {
      "cols" => (&mut term.cols, 20, 500),
      "rows" => (&mut term.rows, 10, 200),
      _ => {
        errors.push(format!("unknown key 'terminal.{k}' (allowed: cols, rows)"));
        continue;
      }
    };
    match v {
      Node::Map(_) => errors.push(format!("'terminal.{k}' must be a number, not a mapping")),
      Node::Scalar(s) => match s.trim().parse::<u16>() {
        Ok(n) if (min..=max).contains(&n) => *target = n,
        Ok(n) => errors.push(format!("'terminal.{k}' is {n}, outside the allowed range {min}..={max}")),
        Err(_) => errors.push(format!("'terminal.{k}' must be a number, got '{}'", s.trim())),
      },
    }
  }
  term
}

/// Validate one named skill's fields (`harness` required+known, `model` optional,
/// `result` required+repo-relative), pushing every problem found. Returns the
/// built [`Skill`] only when it is fully valid.
fn validate_skill(name: &str, fields: &[(String, Node)], errors: &mut Vec<String>) -> Option<Skill> {
  let mut fm: BTreeMap<&str, &Node> = BTreeMap::new();
  for (k, v) in fields {
    if fm.insert(k.as_str(), v).is_some() {
      errors.push(format!("duplicate key 'skills.{name}.{k}'"));
    }
  }
  const SK: &[&str] =
    &["harness", "model", "effort", "timeout", "env", "profile", "commits", "autoinstall", "invocations", "result"];
  for (k, _) in fields {
    if !SK.contains(&k.as_str()) {
      errors.push(format!(
        "unknown key 'skills.{name}.{k}' (allowed: harness, model, effort, timeout, env, profile, commits, autoinstall, invocations, result)"
      ));
    }
  }
  if fm.contains_key("skill") {
    errors.push(format!("'skills.{name}.skill' is not allowed — the skill key must match the .skills/<name>/ folder"));
  }

  // harness: required for direct runs; forbidden when `invocations:` is set.
  let harness = match fm.get("harness").copied() {
    None => None,
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{name}.harness' must be a string, not a mapping"));
      None
    }
    Some(Node::Scalar(s)) => match Harness::parse(s.trim()) {
      Some(h) => Some(h),
      None => {
        errors.push(format!(
          "'skills.{name}.harness' is '{}', not a known harness (known: {})",
          s.trim(),
          Harness::known().join(", ")
        ));
        None
      }
    },
  };

  // model: optional string (direct runs only).
  let model = match fm.get("model").copied() {
    None => None,
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{name}.model' must be a string, not a mapping"));
      None
    }
    Some(Node::Scalar(s)) => {
      let s = s.trim();
      if s.is_empty() {
        errors.push(format!("'skills.{name}.model' must not be empty (omit the key for the harness default)"));
        None
      } else {
        Some(s.to_string())
      }
    }
  };

  // effort: optional reasoning-effort level; only harnesses with an effort knob accept
  // an explicit direct-run value. With `invocations:` it is a default routes may override
  // (routes whose harness has no effort knob ignore the inherited value).
  let effort = match fm.get("effort").copied() {
    None => None,
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{name}.effort' must be a string, not a mapping"));
      None
    }
    Some(Node::Scalar(s)) => {
      let s = s.trim();
      if s.is_empty() {
        errors.push(format!("'skills.{name}.effort' must not be empty (omit the key for the model default)"));
        None
      } else {
        Some(s.to_string())
      }
    }
  };

  // invocations: optional matrix — expands to `{name}-{route}` at run and install time.
  let invocations = match fm.get("invocations").copied() {
    None => Vec::new(),
    Some(node) => validate_invocations(name, node, errors),
  };
  if !invocations.is_empty() {
    if harness.is_some() {
      errors.push(format!("'skills.{name}' must declare either 'harness:' or 'invocations:', not both"));
    }
    if model.is_some() {
      errors.push(format!("'skills.{name}.model' must not be set when 'invocations:' is used (set model per route)"));
    }
    if let Some(e) = &effort {
      // A skill-level default must at least be a level SOME harness accepts.
      if !known_effort_level(e) {
        errors.push(format!(
          "'skills.{name}.effort' is '{e}', not a known effort level (known: minimal, low, medium, high, xhigh, max)"
        ));
      }
    }
  } else if harness.is_none() {
    errors.push(format!("skill '{name}' is missing required key 'harness' (or declare 'invocations:')"));
  } else if let (Some(h), Some(e)) = (harness, &effort) {
    check_effort_for_harness(&format!("skills.{name}.effort"), h, e, errors);
  }

  // result: required, repo-relative safe path.
  let result = match fm.get("result").copied() {
    None => {
      errors.push(format!("skill '{name}' is missing required key 'result' (the output file it must produce)"));
      None
    }
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{name}.result' must be a string, not a mapping"));
      None
    }
    Some(Node::Scalar(s)) => {
      let s = s.trim();
      if s.is_empty() {
        errors.push(format!("'skills.{name}.result' must not be empty"));
        None
      } else if !is_safe_relative(s) {
        errors
          .push(format!("'skills.{name}.result' must be a path inside the repo (got '{s}'): no leading '/', no '..'"));
        None
      } else {
        Some(s.to_string())
      }
    }
  };

  // timeout: optional positive integer (seconds).
  let timeout = match fm.get("timeout").copied() {
    None => None,
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{name}.timeout' must be an integer number of seconds, not a mapping"));
      None
    }
    Some(Node::Scalar(s)) => match s.trim().parse::<u64>() {
      Ok(n) if n >= 1 => Some(n),
      Ok(_) => {
        errors.push(format!("'skills.{name}.timeout' must be a positive number of seconds"));
        None
      }
      Err(_) => {
        errors.push(format!("'skills.{name}.timeout' must be an integer number of seconds (got '{}')", s.trim()));
        None
      }
    },
  };

  // env: optional list/mapping of forwarded variables.
  let env = match fm.get("env").copied() {
    None => Vec::new(),
    Some(Node::Scalar(_)) => {
      errors.push(format!("'skills.{name}.env' must be a list of `KEY: <spec>` entries"));
      Vec::new()
    }
    Some(Node::Map(entries)) => validate_env(name, entries, errors),
  };

  // profile: optional non-empty string; a skill in a profile runs only under
  // `scsh run --profile <name>`, not by default.
  let profile = match fm.get("profile").copied() {
    None => None,
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{name}.profile' must be a string, not a mapping"));
      None
    }
    Some(Node::Scalar(s)) => {
      let s = s.trim();
      if s.is_empty() {
        errors.push(format!("'skills.{name}.profile' must not be empty (omit the key to run the skill by default)"));
        None
      } else {
        Some(s.to_string())
      }
    }
  };

  // commits: optional boolean (default false). When true, scsh brings commits the
  // skill makes in its clone back into the caller repo (rebased, or saved to a branch).
  let commits = match fm.get("commits").copied() {
    None => false,
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{name}.commits' must be true or false, not a mapping"));
      false
    }
    Some(Node::Scalar(s)) => match s.trim() {
      "true" => true,
      "false" => false,
      other => {
        errors.push(format!("'skills.{name}.commits' must be true or false (got '{other}')"));
        false
      }
    },
  };

  // autoinstall: optional boolean (default true). Consulted only by installskills/
  // updateskills when this repo is used as an install source; `false` keeps the skill
  // authoring-only (scsh won't install it or add it to a consumer's .scsh.yml).
  let autoinstall = match fm.get("autoinstall").copied() {
    None => true,
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{name}.autoinstall' must be true or false, not a mapping"));
      true
    }
    Some(Node::Scalar(s)) => match s.trim() {
      "true" => true,
      "false" => false,
      other => {
        errors.push(format!("'skills.{name}.autoinstall' must be true or false (got '{other}')"));
        true
      }
    },
  };

  match result {
    Some(result) => {
      if !invocations.is_empty() && !result.contains("{name}") {
        errors.push(format!(
          "'skills.{name}.result' must contain '{{name}}' when 'invocations:' is set (each route substitutes its name)"
        ));
      }
      Some(Skill {
        name: name.to_string(),
        harness,
        model,
        effort,
        timeout,
        env,
        profile,
        commits,
        autoinstall,
        invocations,
        result,
      })
    }
    _ => None,
  }
}

/// Validate a skill's `invocations:` block. Shared with the harness-definition
/// parser (`.harness/<name>.yml`), whose `invocations:` uses the identical schema.
pub(crate) fn validate_invocations(skill: &str, node: &Node, errors: &mut Vec<String>) -> Vec<InvocationRoute> {
  let entries = match node {
    Node::Map(m) => m,
    Node::Scalar(_) => {
      errors.push(format!("'skills.{skill}.invocations' must list one or more named routes"));
      return Vec::new();
    }
  };
  if entries.is_empty() {
    errors.push(format!("'skills.{skill}.invocations' must list at least one route"));
    return Vec::new();
  }
  let mut out = Vec::new();
  let mut seen: BTreeMap<String, ()> = BTreeMap::new();
  for (raw_key, child) in entries {
    let default_name = raw_key.strip_prefix("- ").unwrap_or(raw_key.as_str()).trim().to_string();
    if default_name.is_empty() {
      errors.push(format!("'skills.{skill}.invocations' entry must have a route name"));
      continue;
    }
    let fields = match child {
      Node::Map(f) => f,
      Node::Scalar(_) => {
        errors.push(format!("'skills.{skill}.invocations.{default_name}' must be a mapping with 'harness'"));
        continue;
      }
    };
    let mut fm: BTreeMap<&str, &Node> = BTreeMap::new();
    for (k, v) in fields {
      if fm.insert(k.as_str(), v).is_some() {
        errors.push(format!("duplicate key 'skills.{skill}.invocations.{default_name}.{k}'"));
      }
    }
    const IK: &[&str] = &["name", "harness", "model", "effort", "profile", "commits"];
    for (k, _) in fields {
      if !IK.contains(&k.as_str()) {
        errors.push(format!(
          "unknown key 'skills.{skill}.invocations.{default_name}.{k}' (allowed: name, harness, model, effort, profile, commits)"
        ));
      }
    }
    let route_name = match fm.get("name").copied() {
      None => default_name.clone(),
      Some(Node::Map(_)) => {
        errors.push(format!("'skills.{skill}.invocations.{default_name}.name' must be a string, not a mapping"));
        default_name.clone()
      }
      Some(Node::Scalar(s)) => {
        let s = s.trim();
        if s.is_empty() {
          errors.push(format!("'skills.{skill}.invocations.{default_name}.name' must not be empty"));
          default_name.clone()
        } else {
          s.to_string()
        }
      }
    };
    if seen.insert(route_name.clone(), ()).is_some() {
      errors.push(format!("'skills.{skill}.invocations' has duplicate route '{route_name}'"));
      continue;
    }
    let harness = match fm.get("harness").copied() {
      None => {
        errors.push(format!("'skills.{skill}.invocations.{default_name}' is missing required key 'harness'"));
        None
      }
      Some(Node::Map(_)) => {
        errors.push(format!("'skills.{skill}.invocations.{default_name}.harness' must be a string, not a mapping"));
        None
      }
      Some(Node::Scalar(s)) => match Harness::parse(s.trim()) {
        Some(h) => Some(h),
        None => {
          errors.push(format!(
            "'skills.{skill}.invocations.{default_name}.harness' is '{}', not a known harness (known: {})",
            s.trim(),
            Harness::known().join(", ")
          ));
          None
        }
      },
    };
    let model = parse_optional_string_field(
      skill,
      &format!("invocations.{default_name}.model"),
      fm.get("model").copied(),
      errors,
      "omit the key for the harness default",
    );
    let effort = parse_optional_string_field(
      skill,
      &format!("invocations.{default_name}.effort"),
      fm.get("effort").copied(),
      errors,
      "omit the key for the model default",
    );
    if let (Some(h), Some(e)) = (harness, &effort) {
      check_effort_for_harness(&format!("skills.{skill}.invocations.{default_name}.effort"), h, e, errors);
    }
    let profile = parse_optional_string_field(
      skill,
      &format!("invocations.{default_name}.profile"),
      fm.get("profile").copied(),
      errors,
      "omit the key to inherit the skill-level profile",
    );
    let commits = match fm.get("commits").copied() {
      None => None,
      Some(Node::Map(_)) => {
        errors
          .push(format!("'skills.{skill}.invocations.{default_name}.commits' must be true or false, not a mapping"));
        None
      }
      Some(Node::Scalar(s)) => match s.trim() {
        "true" => Some(true),
        "false" => Some(false),
        other => {
          errors
            .push(format!("'skills.{skill}.invocations.{default_name}.commits' must be true or false (got '{other}')"));
          None
        }
      },
    };
    if let Some(harness) = harness {
      out.push(InvocationRoute { name: route_name, harness, model, effort, profile, commits });
    }
  }
  out
}

/// True when `level` is an effort level at least one harness accepts.
fn known_effort_level(level: &str) -> bool {
  matches!(level, "minimal" | "low" | "medium" | "high" | "xhigh" | "max")
}

/// An EXPLICIT effort on a specific harness must be a level that harness's CLI accepts.
fn check_effort_for_harness(field: &str, harness: Harness, effort: &str, errors: &mut Vec<String>) {
  let levels = harness.effort_levels();
  if levels.is_empty() {
    errors.push(format!(
      "'{field}' is set, but harness '{}' has no effort knob (effort works with: codex, grok, cursor)",
      harness.as_str()
    ));
  } else if !levels.contains(&effort) {
    errors.push(format!(
      "'{field}' is '{effort}', not a level {} accepts (accepted: {})",
      harness.as_str(),
      levels.join(", ")
    ));
  }
}

fn parse_optional_string_field(
  skill: &str, field: &str, node: Option<&Node>, errors: &mut Vec<String>, empty_hint: &str,
) -> Option<String> {
  match node {
    None => None,
    Some(Node::Map(_)) => {
      errors.push(format!("'skills.{skill}.{field}' must be a string, not a mapping"));
      None
    }
    Some(Node::Scalar(s)) => {
      let s = s.trim();
      if s.is_empty() {
        errors.push(format!("'skills.{skill}.{field}' must not be empty ({empty_hint})"));
        None
      } else {
        Some(s.to_string())
      }
    }
  }
}

/// Validate a skill's `env:` block — accepting both the list form (`- KEY: spec`,
/// which tokenizes with a leading `- ` on the key) and a plain `KEY: spec` mapping
/// — into the forwarded variables, pushing every problem found.
fn validate_env(skill: &str, entries: &[(String, Node)], errors: &mut Vec<String>) -> Vec<EnvVar> {
  let mut out = Vec::new();
  let mut seen: BTreeMap<String, ()> = BTreeMap::new();
  for (raw_key, node) in entries {
    let key = raw_key.strip_prefix("- ").unwrap_or(raw_key).trim().to_string();
    if !is_env_name(&key) {
      errors.push(format!("'skills.{skill}.env': '{key}' is not a valid environment variable name"));
      continue;
    }
    if seen.insert(key.clone(), ()).is_some() {
      errors.push(format!("'skills.{skill}.env': duplicate variable '{key}'"));
    }
    let spec = match node {
      Node::Scalar(s) => s.trim(),
      Node::Map(_) => {
        errors.push(format!("'skills.{skill}.env.{key}' must be a string spec, not a mapping"));
        continue;
      }
    };
    match parse_env_spec(spec) {
      Ok(rule) => out.push(EnvVar { key, rule }),
      Err(e) => errors.push(format!("'skills.{skill}.env.{key}': {e}")),
    }
  }
  out
}

/// Parse the value side of an `env:` entry into how scsh resolves it:
///
/// * `${VAR}` / `$VAR` → **required** (refuse the skill if the host var is unset),
/// * `${VAR:-default}` → forward the host var or inject `default` (`${VAR:-}` = empty),
/// * `${VAR:?message}` → **required**, refusing with the custom `message`,
/// * anything without a `$…` reference → a literal constant.
fn parse_env_spec(value: &str) -> Result<EnvRule, String> {
  const SHAPE: &str = "an env value must be a literal, ${VAR}, ${VAR:-default}, or ${VAR:?message}";
  let value = value.trim();
  if let Some(inner) = value.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
    let inner = inner.trim();
    match inner.find(':') {
      // Bare ${VAR}: required, with the default "you must provide it" message.
      None => {
        if is_env_name(inner) {
          Ok(EnvRule::Require { src: inner.to_string(), message: required_message(inner) })
        } else {
          Err(format!("'{inner}' is not a valid environment variable name"))
        }
      }
      Some(i) => {
        let src = inner[..i].trim().to_string();
        if !is_env_name(&src) {
          return Err(format!("'{src}' is not a valid environment variable name"));
        }
        let rest = &inner[i + 1..];
        if let Some(default) = rest.strip_prefix('-') {
          Ok(EnvRule::Default { src, default: default.to_string() })
        } else if let Some(message) = rest.strip_prefix('?') {
          Ok(EnvRule::Require { src, message: message.trim().to_string() })
        } else {
          Err(SHAPE.to_string())
        }
      }
    }
  } else if let Some(name) = bare_var(value) {
    // Unbraced whole-value $VAR: required, same default message as bare ${VAR}.
    Ok(EnvRule::Require { src: name.to_string(), message: required_message(name) })
  } else if value.contains("${") {
    // A broken expansion (e.g. missing braces) — reject rather than treat as literal.
    Err(SHAPE.to_string())
  } else {
    Ok(EnvRule::Constant(value.to_string()))
  }
}

/// `Some(name)` when `value` is exactly `$NAME` with a valid env-var name (so a plain
/// literal like `$5` or `price$x` stays a constant, but `$A` is a variable reference).
fn bare_var(value: &str) -> Option<&str> {
  let name = value.strip_prefix('$')?;
  is_env_name(name).then_some(name)
}

/// The default "you must define this" message for a bare `${VAR}` / `$VAR` reference.
fn required_message(name: &str) -> String {
  format!(
    "Environmental variable {name} is not provided, use the ${{{name}:-}} syntax to allow for empty values as defaults"
  )
}

/// Whether `s` is a valid POSIX-ish environment variable name (`[A-Za-z_][A-Za-z0-9_]*`).
/// Shared with the harness-definition parser: a param name must be a valid env var
/// name, because each param is forwarded to the container as an environment variable.
pub(crate) fn is_env_name(s: &str) -> bool {
  let mut chars = s.chars();
  match chars.next() {
    Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
    _ => return false,
  }
  chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `p` is a safe path *inside* the repository: relative (no leading `/`),
/// with no `..` component and no backslash, so a skill's result can never be
/// written or copied outside the repo root.
pub fn is_safe_relative(p: &str) -> bool {
  if p.is_empty() || p.starts_with('/') || p.contains('\\') || p.contains('\0') {
    return false;
  }
  p.split('/').all(|c| c != ".." && c != ".")
}

// ---------------------------------------------------------------------------
// The minimal YAML reader
// ---------------------------------------------------------------------------

pub(crate) fn parse_yaml(src: &str) -> Result<Vec<(String, Node)>, String> {
  let raw: Vec<&str> = src.lines().collect();
  let mut lines = Vec::new();
  let mut i = 0;
  while i < raw.len() {
    let lineno = i + 1;
    let content = strip_comment(raw[i]);
    let trimmed = content.trim_end();
    if trimmed.trim().is_empty() {
      i += 1;
      continue;
    }
    let indent = trimmed.len() - trimmed.trim_start().len();
    if trimmed[..indent].contains('\t') {
      return Err(format!("line {lineno}: tab in indentation; use spaces"));
    }
    let body = &trimmed[indent..];
    let colon = find_colon(body).ok_or_else(|| format!("line {lineno}: expected 'key: value'"))?;
    let key = body[..colon].trim().to_string();
    if key.is_empty() {
      return Err(format!("line {lineno}: empty key"));
    }
    let rest = body[colon + 1..].trim();
    // A literal block scalar — `key: |` (keep one trailing newline) or `key: |-` (strip
    // trailing blank lines). Its content is taken verbatim from the raw lines, with no
    // comment stripping and no colon parsing, so a `task:` prompt may contain prose, `#`,
    // and `:` freely. Content lines are those indented deeper than the introducer.
    if rest == "|" || rest == "|-" {
      let (text, next) = read_block_scalar(&raw, i + 1, indent, rest == "|");
      lines.push(Line { indent, key, inline: Some(text), lineno });
      i = next;
      continue;
    }
    let inline = if rest.is_empty() { None } else { Some(unquote(rest)) };
    lines.push(Line { indent, key, inline, lineno });
    i += 1;
  }

  let mut idx = 0;
  let entries = parse_block(&lines, &mut idx, 0)?;
  if idx != lines.len() {
    return Err(format!("line {}: unexpected indentation", lines[idx].lineno));
  }
  Ok(entries)
}

/// Read a literal block scalar. `start` is the first raw line after the `key: |`
/// introducer, which itself sat at `parent_indent`. Content is every following line
/// indented deeper than the parent (blank lines included); the block's base indent is
/// that of its first non-blank line and is stripped from each line (a deeper line keeps
/// its extra indentation). Returns the joined text and the index of the first line past
/// the block. `keep_trailing` (`|`) leaves one trailing newline; `|-` leaves none.
fn read_block_scalar(raw: &[&str], start: usize, parent_indent: usize, keep_trailing: bool) -> (String, usize) {
  let mut base: Option<usize> = None;
  let mut collected: Vec<String> = Vec::new();
  let mut j = start;
  while j < raw.len() {
    let line = raw[j];
    if line.trim().is_empty() {
      collected.push(String::new());
      j += 1;
      continue;
    }
    let ind = line.len() - line.trim_start().len();
    if ind <= parent_indent {
      break;
    }
    let b = *base.get_or_insert(ind);
    // Leading indentation is ASCII spaces, so the byte offset equals the column.
    collected.push(if ind >= b { line[b..].to_string() } else { line.trim_start().to_string() });
    j += 1;
  }
  // Blank lines captured past the real content (block-to-sibling separators, EOF) are dropped.
  while collected.last().is_some_and(|s| s.is_empty()) {
    collected.pop();
  }
  let mut text = collected.join("\n");
  if keep_trailing && !text.is_empty() {
    text.push('\n');
  }
  (text, j)
}

fn parse_block(lines: &[Line], idx: &mut usize, indent: usize) -> Result<Vec<(String, Node)>, String> {
  let mut entries = Vec::new();
  while *idx < lines.len() {
    let line = &lines[*idx];
    if line.indent < indent {
      break;
    }
    if line.indent > indent {
      return Err(format!("line {}: unexpected indentation", line.lineno));
    }
    let key = line.key.clone();
    match &line.inline {
      Some(v) => {
        entries.push((key, Node::Scalar(v.clone())));
        *idx += 1;
      }
      None => {
        *idx += 1;
        if *idx < lines.len() && lines[*idx].indent > indent {
          let child_indent = lines[*idx].indent;
          let children = parse_block(lines, idx, child_indent)?;
          entries.push((key, Node::Map(children)));
        } else {
          // A key with no inline value and no deeper lines: empty mapping.
          entries.push((key, Node::Map(Vec::new())));
        }
      }
    }
  }
  Ok(entries)
}

/// Strip a trailing/whole-line `#` comment, ignoring `#` inside quotes. A `#`
/// only starts a comment when at the start of the line or preceded by space.
fn strip_comment(line: &str) -> String {
  let mut out = String::new();
  let (mut in_s, mut in_d, mut prev_ws) = (false, false, true);
  for c in line.chars() {
    match c {
      '\'' if !in_d => {
        in_s = !in_s;
        out.push(c);
        prev_ws = false;
      }
      '"' if !in_s => {
        in_d = !in_d;
        out.push(c);
        prev_ws = false;
      }
      '#' if !in_s && !in_d && prev_ws => break,
      _ => {
        prev_ws = c.is_whitespace();
        out.push(c);
      }
    }
  }
  out
}

/// Byte offset of the first `:` that is not inside quotes.
fn find_colon(s: &str) -> Option<usize> {
  let (mut in_s, mut in_d) = (false, false);
  for (i, c) in s.char_indices() {
    match c {
      '\'' if !in_d => in_s = !in_s,
      '"' if !in_s => in_d = !in_d,
      ':' if !in_s && !in_d => return Some(i),
      _ => {}
    }
  }
  None
}

/// Remove matching surrounding quotes and apply minimal escape handling.
fn unquote(s: &str) -> String {
  let b = s.as_bytes();
  if s.len() >= 2 {
    let (first, last) = (b[0], b[s.len() - 1]);
    if first == b'"' && last == b'"' {
      return unescape_double(&s[1..s.len() - 1]);
    }
    if first == b'\'' && last == b'\'' {
      return s[1..s.len() - 1].replace("''", "'");
    }
  }
  s.to_string()
}

fn unescape_double(s: &str) -> String {
  let mut out = String::new();
  let mut chars = s.chars();
  while let Some(c) = chars.next() {
    if c == '\\' {
      match chars.next() {
        Some('"') => out.push('"'),
        Some('\\') => out.push('\\'),
        Some('n') => out.push('\n'),
        Some('t') => out.push('\t'),
        Some(other) => {
          out.push('\\');
          out.push(other);
        }
        None => out.push('\\'),
      }
    } else {
      out.push(c);
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A minimal valid config with a single `s` skill, for the negative tests.
  fn one_skill(body: &str) -> String {
    format!(
      r#"skills:
  s:
{body}"#
    )
  }

  #[test]
  fn block_scalar_preserves_multiline_text() {
    let src = "task: |\n  line one\n  line two\n\n  after blank\nnext: x\n";
    let entries = parse_yaml(src).unwrap();
    let task = &entries.iter().find(|(k, _)| k == "task").unwrap().1;
    assert_eq!(*task, Node::Scalar("line one\nline two\n\nafter blank\n".into()));
    // A sibling key after the block is still parsed normally.
    let next = &entries.iter().find(|(k, _)| k == "next").unwrap().1;
    assert_eq!(*next, Node::Scalar("x".into()));
  }

  #[test]
  fn block_scalar_strip_variant_drops_trailing_newline() {
    let entries = parse_yaml("task: |-\n  only line\n").unwrap();
    assert_eq!(entries[0].1, Node::Scalar("only line".into()));
  }

  #[test]
  fn terminal_defaults_to_200x50() {
    let cfg = validate(&one_skill("    harness: opencode\n    result: tmp/x.json\n")).unwrap();
    assert_eq!(cfg.terminal, Terminal { cols: 200, rows: 50 });
  }

  #[test]
  fn terminal_block_overrides_cols_and_rows() {
    let yaml =
      format!("terminal:\n  cols: 120\n  rows: 30\n{}", one_skill("    harness: opencode\n    result: tmp/x.json\n"));
    let cfg = validate(&yaml).unwrap();
    assert_eq!(cfg.terminal, Terminal { cols: 120, rows: 30 });
  }

  #[test]
  fn terminal_partial_override_keeps_other_default() {
    let yaml = format!("terminal:\n  rows: 40\n{}", one_skill("    harness: opencode\n    result: tmp/x.json\n"));
    let cfg = validate(&yaml).unwrap();
    assert_eq!(cfg.terminal, Terminal { cols: 200, rows: 40 });
  }

  #[test]
  fn terminal_rejects_bad_values() {
    let base = one_skill("    harness: opencode\n    result: tmp/x.json\n");
    let errs = validate(&format!("terminal:\n  cols: 5000\n{base}")).unwrap_err();
    assert!(
      errs.iter().any(|e| e.contains("'terminal.cols' is 5000, outside the allowed range 20..=500")),
      "got {errs:?}"
    );
    let errs = validate(&format!("terminal:\n  rows: wide\n{base}")).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("'terminal.rows' must be a number, got 'wide'")), "got {errs:?}");
    let errs = validate(&format!("terminal:\n  depth: 3\n{base}")).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("unknown key 'terminal.depth' (allowed: cols, rows)")), "got {errs:?}");
    let errs = validate(&format!("terminal: big\n{base}")).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("'terminal' must be a mapping")), "got {errs:?}");
  }

  #[test]
  fn skill_key_must_match_folder_skill_field_rejected() {
    let yaml = r#"skills:
  add-gpt:
    skill: add
    harness: opencode
    result: tmp/x.json
"#;
    assert!(validate(yaml).unwrap_err().iter().any(|e| e.contains("'skills.add-gpt.skill' is not allowed")));
  }

  #[test]
  fn claude_harness_is_valid() {
    let yaml = r#"skills:
  x:
    harness: claude
    result: tmp/x.json
"#;
    assert_eq!(validate(yaml).unwrap().skills[0].harness, Some(Harness::Claude));
  }

  #[test]
  fn effort_validates_per_harness_and_inherits_where_supported() {
    // Direct grok skill with a valid effort.
    let yaml = r#"skills:
  x:
    harness: grok
    model: grok-build
    effort: high
    result: tmp/x.json
"#;
    let cfg = validate(yaml).unwrap();
    assert_eq!(cfg.skills[0].effort.as_deref(), Some("high"));
    assert_eq!(expand_invocations(&cfg)[0].effort.as_deref(), Some("high"));

    // Effort on a harness without an effort knob is an error.
    let yaml = r#"skills:
  x:
    harness: claude
    effort: high
    result: tmp/x.json
"#;
    assert!(validate(yaml).unwrap_err().iter().any(|e| e.contains("no effort knob")));

    // A level the harness's CLI rejects is an error (grok has max; codex does not).
    let yaml = r#"skills:
  x:
    harness: codex
    effort: max
    result: tmp/x.json
"#;
    assert!(validate(yaml).unwrap_err().iter().any(|e| e.contains("not a level codex accepts")));

    // Skill-level default over a mixed matrix: applied to codex/grok, ignored for claude.
    let yaml = r#"skills:
  review:
    effort: high
    result: tmp/review-{name}.json
    invocations:
      codex:
        harness: codex
        model: gpt-5.5
      grok:
        harness: grok
        model: grok-build
        effort: xhigh
      claude:
        harness: claude
        model: claude-opus-4-8
"#;
    let inv = expand_invocations(&validate(yaml).unwrap());
    assert_eq!(inv[0].effort.as_deref(), Some("high"), "codex inherits the skill default");
    assert_eq!(inv[1].effort.as_deref(), Some("xhigh"), "grok route override wins");
    assert_eq!(inv[2].effort, None, "claude has no effort knob — inherited value ignored");
  }

  #[test]
  fn grok_harness_is_valid() {
    let yaml = r#"skills:
  x:
    harness: grok
    model: grok-build
    result: tmp/x.json
"#;
    assert_eq!(validate(yaml).unwrap().skills[0].harness, Some(Harness::Grok));
    assert!(Harness::known().contains(&"grok"));
  }

  #[test]
  fn cursor_harness_is_valid() {
    let yaml = r#"skills:
  x:
    harness: cursor
    model: composer-2.5
    effort: high
    result: tmp/x.json
"#;
    let cfg = validate(yaml).unwrap();
    assert_eq!(cfg.skills[0].harness, Some(Harness::Cursor));
    assert_eq!(cfg.skills[0].model.as_deref(), Some("composer-2.5"));
    assert_eq!(cfg.skills[0].effort.as_deref(), Some("high"));
    let inv = expand_invocations(&cfg);
    assert_eq!(inv[0].harness, Harness::Cursor);
    assert_eq!(inv[0].effort.as_deref(), Some("high"));
    assert!(Harness::known().contains(&"cursor"));
  }

  #[test]
  fn codex_harness_is_valid_directly_and_in_invocations() {
    let yaml = r#"skills:
  x:
    harness: codex
    model: gpt-5.5
    result: tmp/x.json
"#;
    let cfg = validate(yaml).unwrap();
    assert_eq!(cfg.skills[0].harness, Some(Harness::Codex));
    assert_eq!(cfg.skills[0].model.as_deref(), Some("gpt-5.5"));
    let yaml = r#"skills:
  review:
    result: tmp/review-{name}.json
    invocations:
      codex-gpt-5.5:
        harness: codex
        model: gpt-5.5
"#;
    let cfg = validate(yaml).unwrap();
    let inv = expand_invocations(&cfg);
    assert_eq!(inv[0].harness, Harness::Codex);
    assert_eq!(inv[0].name, "review-codex-gpt-5.5");
    assert!(Harness::known().contains(&"codex"));
  }

  #[test]
  fn demo_config_is_valid() {
    let cfg = validate(demo_yaml()).expect("demo config should validate");
    assert_eq!(cfg.skills.len(), 2);
    let add = cfg.skills.iter().find(|s| s.name == "add").expect("add present");
    assert_eq!(add.invocations.len(), 2);
    let expanded = expand_invocations(&cfg);
    assert_eq!(expanded.len(), 4);
    let add_oc =
      expanded.iter().find(|s| s.name == "add-opencode-gpt-5.4-mini-fast").expect("add-opencode-gpt-5.4-mini-fast");
    assert_eq!(add_oc.skill_source, "add");
    assert_eq!(add_oc.harness, Harness::Opencode);
    assert_eq!(add_oc.model.as_deref(), Some("openai/gpt-5.4-mini-fast"));
    assert_eq!(add_oc.result, "tmp/add_opencode-gpt-5.4-mini-fast_result.json");
    assert!(add_oc.commits, "add-opencode-gpt-5.4-mini-fast is commit-enabled");
    let add_cl = expanded.iter().find(|s| s.name == "add-claude-sonnet-4-6").expect("add-claude-sonnet-4-6");
    assert!(!add_cl.commits);
    let mul_oc =
      expanded.iter().find(|s| s.name == "multiply-opencode-gpt-5.4-mini-fast").expect("multiply-opencode-gpt");
    assert_eq!(mul_oc.profile.as_deref(), Some("multiply"));
  }

  #[test]
  fn bundled_skills_ship_the_demo_selftest() {
    let skills = bundled_skills();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].0, ".skills/scsh-harness-demo-and-selftest/SKILL.md");
    assert!(
      skills[0].1.contains("name: scsh-harness-demo-and-selftest"),
      "the bundled skill should be the demo/self-test"
    );
  }

  #[test]
  fn commits_is_an_optional_boolean() {
    let yes = one_skill(
      r#"    harness: opencode
    commits: true
    result: tmp/x.json
"#,
    );
    assert!(validate(&yes).unwrap().skills[0].commits);
    let no = one_skill(
      r#"    harness: opencode
    commits: false
    result: tmp/x.json
"#,
    );
    assert!(!validate(&no).unwrap().skills[0].commits);
    let default = one_skill(
      r#"    harness: opencode
    result: tmp/x.json
"#,
    );
    assert!(!validate(&default).unwrap().skills[0].commits, "commits defaults to false");
    let bad = one_skill(
      r#"    harness: opencode
    commits: yes
    result: tmp/x.json
"#,
    );
    assert!(validate(&bad).unwrap_err().iter().any(|e| e.contains("'skills.s.commits' must be true or false")));
  }

  #[test]
  fn autoinstall_is_an_optional_boolean_defaulting_true() {
    let default = one_skill(
      r#"    harness: opencode
    result: tmp/x.json
"#,
    );
    assert!(validate(&default).unwrap().skills[0].autoinstall, "autoinstall defaults to true");
    let no = one_skill(
      r#"    harness: opencode
    autoinstall: false
    result: tmp/x.json
"#,
    );
    assert!(!validate(&no).unwrap().skills[0].autoinstall);
    let yes = one_skill(
      r#"    harness: opencode
    autoinstall: true
    result: tmp/x.json
"#,
    );
    assert!(validate(&yes).unwrap().skills[0].autoinstall);
    let bad = one_skill(
      r#"    harness: opencode
    autoinstall: nope
    result: tmp/x.json
"#,
    );
    assert!(validate(&bad).unwrap_err().iter().any(|e| e.contains("'skills.s.autoinstall' must be true or false")));
  }

  #[test]
  fn extract_skill_block_keeps_fields_and_drops_autoinstall() {
    let yaml = r#"skills:
  alpha:
    autoinstall: false
    harness: opencode
    profile: rev
    result: tmp/a.json
  beta:
    harness: opencode
    result: tmp/b.json
"#;
    let block = extract_skill_block(yaml, "alpha").expect("alpha present");
    assert!(
      block.contains("  alpha:") && block.contains("    harness: opencode") && block.contains("    profile: rev")
    );
    assert!(!block.contains("autoinstall"), "the autoinstall directive is dropped");
    assert!(!block.contains("beta"), "only alpha's block, not the next skill");
    // The extracted block re-validates as a one-skill manifest.
    let cfg = validate(&format!("skills:\n{block}")).expect("merged block is valid");
    assert_eq!(cfg.skills.len(), 1);
    assert_eq!(cfg.skills[0].name, "alpha");
    assert!(cfg.skills[0].autoinstall, "the merged consumer entry has no autoinstall → defaults true");
    assert!(extract_skill_block(yaml, "missing").is_none());
  }

  #[test]
  fn invocations_expand_for_run() {
    let yaml = r#"skills:
  reviewer:
    profile: code-review
    timeout: 1200
    result: tmp/review-{name}.json
    invocations:
      opencode-gpt-5.5:
        harness: opencode
        model: openai/gpt-5.5
      claude-opus-4-6:
        harness: claude
        model: claude-opus-4-6
"#;
    let cfg = validate(yaml).unwrap();
    let expanded = expand_invocations(&cfg);
    assert_eq!(expanded.len(), 2);
    assert_eq!(expanded[0].name, "reviewer-opencode-gpt-5.5");
    assert_eq!(expanded[0].result, "tmp/review-opencode-gpt-5.5.json");
    assert_eq!(expanded[0].profile.as_deref(), Some("code-review"));
  }

  #[test]
  fn invocations_require_name_placeholder_in_result() {
    let yaml = one_skill(
      r#"    result: tmp/x.json
    invocations:
      gpt:
        harness: opencode
"#,
    );
    assert!(validate(&yaml).unwrap_err().iter().any(|e| e.contains("{name}")));
  }

  #[test]
  fn duplicate_expanded_result_paths_are_rejected() {
    let yaml = r#"skills:
  add:
    result: tmp/{name}.json
    invocations:
      route-a:
        harness: opencode
  multiply:
    result: tmp/{name}.json
    invocations:
      route-a:
        harness: opencode
"#;
    let errs = validate(yaml).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("duplicate result path 'tmp/route-a.json'")), "got {errs:?}");
  }

  #[test]
  fn invocations_profile_override() {
    let yaml = r#"skills:
  reviewer:
    profile: code-review
    result: tmp/review-{name}.json
    invocations:
      special:
        harness: opencode
        profile: special-profile
"#;
    let expanded = expand_invocations(&validate(yaml).unwrap());
    assert_eq!(expanded[0].profile.as_deref(), Some("special-profile"));
  }

  #[test]
  fn extract_skill_block_keeps_invocations() {
    let yaml = r#"skills:
  reviewer:
    profile: code-review
    result: tmp/review-{name}.json
    invocations:
      gpt:
        harness: opencode
        model: openai/gpt-5.5
"#;
    let block = extract_skill_block(yaml, "reviewer").expect("reviewer present");
    assert!(block.contains("invocations:"));
    assert!(block.contains("gpt:"));
  }

  #[test]
  fn profile_is_an_optional_non_empty_string() {
    let with = one_skill(
      r#"    harness: opencode
    profile: full
    result: tmp/x.json
"#,
    );
    assert_eq!(validate(&with).unwrap().skills[0].profile.as_deref(), Some("full"));
    let without = one_skill(
      r#"    harness: opencode
    result: tmp/x.json
"#,
    );
    assert_eq!(validate(&without).unwrap().skills[0].profile, None);
    let empty = one_skill(
      r#"    harness: opencode
    profile: ""
    result: tmp/x.json
"#,
    );
    assert!(validate(&empty).unwrap_err().iter().any(|e| e.contains("'skills.s.profile' must not be empty")));
  }

  #[test]
  fn env_specs_parse_list_and_mapping_forms() {
    // List form (`- KEY: spec`): default-injecting, required (with and without a
    // custom message), bare $VAR, empty default, and a literal constant.
    let list = one_skill(
      r#"    harness: opencode
    env:
      - A: ${A:-5}
      - X: ${X:?need X}
      - R: ${R}
      - S: $S
      - E: ${E:-}
      - LIT: hello
    result: tmp/x.json
"#,
    );
    let env = &validate(&list).unwrap().skills[0].env;
    assert_eq!(env[0], EnvVar { key: "A".into(), rule: EnvRule::Default { src: "A".into(), default: "5".into() } });
    assert_eq!(
      env[1],
      EnvVar { key: "X".into(), rule: EnvRule::Require { src: "X".into(), message: "need X".into() } }
    );
    // Bare ${R} and $S are required, with the default guidance message.
    assert!(matches!(&env[2].rule, EnvRule::Require { src, message } if src == "R" && message.contains("${R:-}")));
    assert!(matches!(&env[3].rule, EnvRule::Require { src, message } if src == "S" && message.contains("${S:-}")));
    assert_eq!(env[4], EnvVar { key: "E".into(), rule: EnvRule::Default { src: "E".into(), default: "".into() } });
    assert_eq!(env[5], EnvVar { key: "LIT".into(), rule: EnvRule::Constant("hello".into()) });

    // Mapping form (`KEY: spec`, no dash) parses identically.
    let map = one_skill(
      r#"    harness: opencode
    env:
      A: ${A:-5}
    result: tmp/x.json
"#,
    );
    assert_eq!(
      validate(&map).unwrap().skills[0].env,
      vec![EnvVar { key: "A".into(), rule: EnvRule::Default { src: "A".into(), default: "5".into() } }]
    );
  }

  #[test]
  fn bare_var_is_required_with_guidance_message() {
    // `${A}` and `$A` both require A and point at the `${A:-}` empty-default syntax.
    for spec in ["${A}", "$A"] {
      let src = one_skill(&format!(
        r#"    harness: opencode
    env:
      - A: {spec}
    result: tmp/x.json
"#
      ));
      let rule = &validate(&src).unwrap().skills[0].env[0].rule;
      match rule {
        EnvRule::Require { src, message } => {
          assert_eq!(src, "A");
          assert!(message.contains("Environmental variable A is not provided"), "got {message}");
          assert!(message.contains("${A:-}"), "message should suggest the empty-default syntax; got {message}");
        }
        other => panic!("expected Require for {spec}, got {other:?}"),
      }
    }
  }

  #[test]
  fn timeout_is_an_optional_positive_integer() {
    let with = one_skill(
      r#"    harness: opencode
    timeout: 30
    result: tmp/x.json
"#,
    );
    assert_eq!(validate(&with).unwrap().skills[0].timeout, Some(30));
    let without = one_skill(
      r#"    harness: opencode
    result: tmp/x.json
"#,
    );
    assert_eq!(validate(&without).unwrap().skills[0].timeout, None);
    let zero = one_skill(
      r#"    harness: opencode
    timeout: 0
    result: tmp/x.json
"#,
    );
    assert!(validate(&zero).unwrap_err().iter().any(|e| e.contains("positive number of seconds")));
    let bad = one_skill(
      r#"    harness: opencode
    timeout: soon
    result: tmp/x.json
"#,
    );
    assert!(validate(&bad).unwrap_err().iter().any(|e| e.contains("integer number of seconds")));
  }

  #[test]
  fn env_rejects_bad_specs_names_and_dups() {
    // A malformed expansion (`:` followed by neither `-` nor `?`) is rejected.
    let bad_spec = one_skill(
      r#"    harness: opencode
    env:
      - A: ${A:@x}
    result: tmp/x.json
"#,
    );
    assert!(validate(&bad_spec).unwrap_err().iter().any(|e| e.contains("env.A")), "got {:?}", validate(&bad_spec));
    let bad_name = one_skill(
      r#"    harness: opencode
    env:
      - 1BAD: ${X:-1}
    result: tmp/x.json
"#,
    );
    assert!(validate(&bad_name).unwrap_err().iter().any(|e| e.contains("not a valid environment variable name")));
    let dup = one_skill(
      r#"    harness: opencode
    env:
      - A: ${A:-1}
      - A: ${A:-2}
    result: tmp/x.json
"#,
    );
    assert!(validate(&dup).unwrap_err().iter().any(|e| e.contains("duplicate variable 'A'")));
  }

  #[test]
  fn parses_two_level_skills_in_file_order() {
    let src = r#"skills:
  build:
    harness: opencode
    result: tmp/out.json
  test:
    harness: opencode
    model: m
    result: tmp/test.json
"#;
    let cfg = validate(src).unwrap();
    assert_eq!(cfg.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["build", "test"]);
    assert_eq!(cfg.skills[0].model, None); // model is optional
    assert_eq!(cfg.skills[1].model.as_deref(), Some("m"));
  }

  #[test]
  fn result_is_required_per_skill() {
    let errs = validate(&one_skill(
      r#"    harness: opencode
"#,
    ))
    .unwrap_err();
    assert!(errs.iter().any(|e| e.contains("missing required key 'result'")), "got {errs:?}");
  }

  #[test]
  fn harness_is_required_and_validated() {
    let missing = validate(&one_skill(
      r#"    result: tmp/x.json
"#,
    ))
    .unwrap_err();
    assert!(missing.iter().any(|e| e.contains("missing required key 'harness'")), "got {missing:?}");
    let unknown = validate(&one_skill(
      r#"    harness: bogus
    result: tmp/x.json
"#,
    ))
    .unwrap_err();
    assert!(unknown.iter().any(|e| e.contains("not a known harness")), "got {unknown:?}");
  }

  #[test]
  fn result_rejects_paths_outside_the_repo() {
    for bad in ["/etc/passwd", "../escape.json", "a/../../b.json"] {
      let errs = validate(&one_skill(&format!(
        r#"    harness: opencode
    result: {bad}
"#
      )))
      .unwrap_err();
      assert!(
        errs.iter().any(|e| e.contains("must be a path inside the repo")),
        "expected a result-path error for {bad}, got {errs:?}"
      );
    }
  }

  #[test]
  fn unknown_skill_key_reported() {
    let errs = validate(&one_skill(
      r#"    harness: opencode
    result: tmp/x.json
    bogus: y
"#,
    ))
    .unwrap_err();
    assert!(errs.iter().any(|e| e.contains("unknown key 'skills.s.bogus'")), "got {errs:?}");
  }

  #[test]
  fn at_least_one_skill_required() {
    let errs = validate("skills:\n").unwrap_err();
    assert!(errs.iter().any(|e| e.contains("at least one skill")), "got {errs:?}");
  }

  #[test]
  fn duplicate_skill_reported() {
    let src = r#"skills:
  s:
    harness: opencode
    result: tmp/a.json
  s:
    harness: opencode
    result: tmp/b.json
"#;
    let errs = validate(src).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("duplicate skill 's'")), "got {errs:?}");
  }

  #[test]
  fn strips_comments_full_line_and_trailing() {
    let src = r#"# a comment
skills:
  s:
    harness: opencode   # trailing comment
    result: tmp/x.json
"#;
    let cfg = validate(src).unwrap();
    assert_eq!(cfg.skills.len(), 1);
    assert_eq!(cfg.skills[0].name, "s");
  }

  #[test]
  fn hash_inside_quotes_is_not_a_comment() {
    let src = one_skill(
      r#"    harness: opencode
    model: "gpt#1"
    result: tmp/x.json
"#,
    );
    let cfg = validate(&src).unwrap();
    assert_eq!(cfg.skills[0].model.as_deref(), Some("gpt#1"));
  }

  #[test]
  fn missing_skills_reported() {
    // An empty (comment-only) file has no skills.
    let errs = validate("# nothing here\n").unwrap_err();
    assert!(errs.iter().any(|e| e.contains("missing required key 'skills'")), "got {errs:?}");
  }

  #[test]
  fn skills_must_be_mapping() {
    let errs = validate("skills: hello\n").unwrap_err();
    assert!(errs.iter().any(|e| e.contains("mapping")));
  }

  #[test]
  fn tui_harnesses_are_the_interactive_ones() {
    // Claude/Codex/Cursor run under tmux+asciinema (their missing-result failures are retryable);
    // Opencode/Grok are headless.
    assert!(Harness::Claude.is_tui());
    assert!(Harness::Codex.is_tui());
    assert!(Harness::Cursor.is_tui());
    assert!(!Harness::Opencode.is_tui());
    assert!(!Harness::Grok.is_tui());
  }

  #[test]
  fn unknown_top_level_key_reported() {
    // version/project/image are no longer part of the schema — they read as unknown.
    let errs = validate(&format!(
      "{}version: 1\n",
      one_skill(
        r#"    harness: opencode
    result: tmp/x.json
"#,
      )
    ))
    .unwrap_err();
    assert!(errs.iter().any(|e| e.contains("unknown top-level key 'version'")), "got {errs:?}");
    assert!(errs.iter().any(|e| e.contains("allowed: skills, terminal")), "got {errs:?}");
  }

  #[test]
  fn duplicate_top_level_key_reported() {
    let src = r#"skills:
  s:
    harness: opencode
    result: tmp/a.json
skills:
  t:
    harness: opencode
    result: tmp/b.json
"#;
    let errs = validate(src).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("duplicate top-level key 'skills'")), "got {errs:?}");
  }

  #[test]
  fn all_problems_collected_at_once() {
    // An unknown top-level key, a skill missing harness AND result, and an unknown
    // skill key — all reported in one pass.
    let errs = validate(
      r#"nope: 1
skills:
  s:
    bogus: y
"#,
    )
    .unwrap_err();
    assert!(errs.len() >= 3, "expected several problems, got {errs:?}");
  }
}
