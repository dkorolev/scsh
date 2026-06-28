//! `.scsh.yml` parsing and schema validation.
//!
//! scsh keeps its own logic dependency-free, so rather than pull in a full YAML
//! library it carries a deliberately small parser that understands just the
//! subset of YAML the config needs: comments, `key: value` pairs, single-quoted
//! / double-quoted scalars, and nested mappings (the `skills:` block nests two
//! levels — skill name, then its fields). Anything outside that subset is
//! reported as an error rather than silently mis-parsed.

use std::collections::BTreeMap;

/// A parsed-and-validated `.scsh.yml`. For v0.1 the file is just its skills — there is no
/// `version`/`project`/`image` boilerplate; the base image (a glibc Debian dev/CLI toolchain)
/// is fixed and owned by the generated [`crate::runtime::dockerfile`] (`src/Dockerfile`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
  /// One or more scoped skills, in file order. scsh runs them all in parallel.
  pub skills: Vec<Skill>,
}

/// A scoped skill: a name (matching a `.skills/<name>/`), the harness that runs
/// it, an optional model the harness passes to its tool, and the `result` file
/// the skill must produce (a repo-relative path). scsh fails the skill's run —
/// and the whole invocation — if the result is missing, and otherwise copies it
/// back into the host repo. The user never writes the container command: the
/// harness builds it from the skill name and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
  pub name: String,
  pub harness: Harness,
  pub model: Option<String>,
  /// Wall-clock limit for the skill's harness run, in seconds. `None` = no limit;
  /// when set, scsh kills the container and fails the skill once it is exceeded.
  pub timeout: Option<u64>,
  /// Host environment variables to forward into the skill's container, in file
  /// order (see [`EnvVar`]). Empty when the skill declares no `env:`.
  pub env: Vec<EnvVar>,
  /// Optional profile this skill belongs to. A skill with no `profile` runs by
  /// default; one with `profile: <name>` runs only when `scsh run --profile <name>` is
  /// given (so skills needing variables that may be absent stay out of the default
  /// run). `None` when the skill declares no `profile:`.
  pub profile: Option<String>,
  /// Whether scsh takes commits this skill makes in its clone back into the caller's
  /// repo. When `true`, after the run scsh looks for commits the skill added to its
  /// clone branch (`base..clone-HEAD`) and rebases them onto the caller's current
  /// branch — or, if they don't apply cleanly, saves them to a distinct
  /// `scsh/incoming/<skill>-…` branch for the user to inspect. `false` (the default)
  /// means scsh only collects the `result` file and ignores any commits. Bringing in
  /// commits is a real, non-idempotent side effect: running again adds them again.
  pub commits: bool,
  /// Whether `scsh installskills`/`updateskills` ship this skill when this repo is used
  /// as an install source. `true` (the default) installs it and adds it to the consumer's
  /// `.scsh.yml`; `false` keeps it authoring-only (e.g. a meta/self-check skill) so scsh
  /// skips it entirely. Consulted only during install — it has no effect on a normal run.
  pub autoinstall: bool,
  pub result: String,
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

/// The built-in harness that runs a skill inside the container. Today only
/// `opencode` exists; this enum is the extension point for more harnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
  Opencode,
}

impl Harness {
  /// Parse a `harness:` value; `None` for an unknown harness.
  pub fn parse(s: &str) -> Option<Harness> {
    match s {
      "opencode" => Some(Harness::Opencode),
      _ => None,
    }
  }

  /// The canonical name, as written in `.scsh.yml`.
  pub fn as_str(self) -> &'static str {
    match self {
      Harness::Opencode => "opencode",
    }
  }

  /// Every known harness name, for error messages.
  pub fn known() -> &'static [&'static str] {
    &["opencode"]
  }
}

/// A node in the tiny YAML tree: either a scalar string or a mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
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
  "# .scsh.yml — Scoped Skills Helper project configuration\n\
     # Generated by `scsh --init-demo-project`. Edit freely; see the README for the schema.\n\
     # The whole file is just your skills — scsh builds them on a built-in base image\n\
     # (Debian, with opencode + a dev toolchain added). Run `scsh help .scsh.yml` for fields.\n\
     \n\
     # One or more scoped skills, keyed by skill name (matching a .skills/<name>/).\n\
     # scsh builds the image once, then runs EVERY skill in parallel under its\n\
     # harness, each in its own container with a fresh clone of this repo mounted.\n\
     #   harness: the built-in scsh harness that runs the skill (only `opencode`\n\
     #            today). The opencode harness runs, inside the container:\n\
     #              opencode -m <model> run \"run skill <name>\"\n\
     #   model:   optional model the harness passes to the tool (omit for its default).\n\
     #   timeout: optional wall-clock limit (seconds); scsh kills the container and\n\
     #            fails the skill if its harness run exceeds it.\n\
     #   env:     optional host variables to forward into the container, each a\n\
     #            `KEY: <spec>`. ${VAR} (or $VAR) requires VAR — scsh refuses the\n\
     #            skill if it is unset; ${VAR:-default} forwards VAR or injects\n\
     #            `default` when unset (${VAR:-} = empty); ${VAR:?message} requires\n\
     #            VAR, refusing with your message. A bare literal sets that literal.\n\
     #   profile: optional; a skill in a profile runs ONLY under `scsh run --profile <name>`,\n\
     #            not by default — use it for skills that need variables which may be unset.\n\
     #   commits: optional true/false (default false). When true, scsh brings commits the\n\
     #            skill makes in its clone back onto your branch (rebased; or saved to a\n\
     #            distinct scsh/incoming/<skill>-… branch if they don't apply cleanly).\n\
     #            It's a real side effect: running again adds the commit(s) again.\n\
     #   result:  a repo-relative path the skill MUST create. scsh fails that skill's\n\
     #            run — and the whole invocation — if it is missing; otherwise it\n\
     #            copies the file back into your repo (backing up any existing one).\n\
     #            Keep it under the gitignored tmp/.\n\
     skills:\n\
     \x20\x20# add forwards A and B with injected defaults (${A:-2}): scsh resolves the\n\
     \x20\x20# value, so the skill always sees A and B and reports their sum. Runs by\n\
     \x20\x20# default — try `scsh run` or `A=10 B=20 scsh run`. It is commit-enabled:\n\
     \x20\x20# it appends the sum to add_log.txt and commits it, and scsh rebases that\n\
     \x20\x20# commit onto your branch (run it twice and you get two commits).\n\
     \x20\x20add:\n\
     \x20\x20\x20\x20harness: opencode\n\
     \x20\x20\x20\x20model: openai/gpt-5.4-mini-fast\n\
     \x20\x20\x20\x20timeout: 600\n\
     \x20\x20\x20\x20commits: true\n\
     \x20\x20\x20\x20env:\n\
     \x20\x20\x20\x20\x20\x20- A: ${A:-2}\n\
     \x20\x20\x20\x20\x20\x20- B: ${B:-3}\n\
     \x20\x20\x20\x20result: tmp/add_result.json\n\
     \x20\x20# multiply needs X and Y (no defaults), so it lives in the `multiply`\n\
     \x20\x20# profile and declares them required (${X}/${Y}). A bare `scsh run` skips\n\
     \x20\x20# it; `X=6 Y=7 scsh run --profile multiply` runs it; without X/Y, scsh\n\
     \x20\x20# itself refuses it before the container starts.\n\
     \x20\x20multiply:\n\
     \x20\x20\x20\x20harness: opencode\n\
     \x20\x20\x20\x20model: openai/gpt-5.4-mini-fast\n\
     \x20\x20\x20\x20timeout: 600\n\
     \x20\x20\x20\x20profile: multiply\n\
     \x20\x20\x20\x20env:\n\
     \x20\x20\x20\x20\x20\x20- X: ${X}\n\
     \x20\x20\x20\x20\x20\x20- Y: ${Y}\n\
     \x20\x20\x20\x20result: tmp/multiply_result.json\n"
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

/// Extract a skill's raw `.scsh.yml` block from `yaml` — the `  <name>:` header line and
/// its indented field lines — with any `autoinstall:` field line removed (that flag is a
/// source-side directive, not part of a consumer's config). Returns `None` if the skill
/// isn't present. `installskills` uses this to copy a source skill's entry verbatim into
/// the consumer's `.scsh.yml`, so its exact fields and env specs survive the merge.
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
        break; // the next skill (indent 2) or a top-level key (indent 0)
      }
      if l.trim_start().starts_with("autoinstall:") {
        continue; // drop the source-only directive
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

  // Index the top-level keys, flagging duplicates. For v0.1 the only key is `skills`.
  let mut top: BTreeMap<&str, &Node> = BTreeMap::new();
  for (k, v) in &entries {
    if top.insert(k.as_str(), v).is_some() {
      errors.push(format!("duplicate top-level key '{k}'"));
    }
  }
  const KNOWN: &[&str] = &["skills"];
  for (k, _) in &entries {
    if !KNOWN.contains(&k.as_str()) {
      errors.push(format!("unknown top-level key '{k}' (the only top-level key is 'skills')"));
    }
  }

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
    Ok(Config { skills })
  } else {
    Err(errors)
  }
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
  const SK: &[&str] = &["harness", "model", "timeout", "env", "profile", "commits", "autoinstall", "result"];
  for (k, _) in fields {
    if !SK.contains(&k.as_str()) {
      errors.push(format!(
        "unknown key 'skills.{name}.{k}' (allowed: harness, model, timeout, env, profile, commits, autoinstall, result)"
      ));
    }
  }

  // harness: required, must name a known harness.
  let harness = match fm.get("harness").copied() {
    None => {
      errors.push(format!("skill '{name}' is missing required key 'harness'"));
      None
    }
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

  // model: optional string.
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

  match (harness, result) {
    (Some(harness), Some(result)) => {
      Some(Skill { name: name.to_string(), harness, model, timeout, env, profile, commits, autoinstall, result })
    }
    _ => None,
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
fn is_env_name(s: &str) -> bool {
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

fn parse_yaml(src: &str) -> Result<Vec<(String, Node)>, String> {
  let mut lines = Vec::new();
  for (i, raw) in src.lines().enumerate() {
    let lineno = i + 1;
    let content = strip_comment(raw);
    let trimmed = content.trim_end();
    if trimmed.trim().is_empty() {
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
    let inline = if rest.is_empty() { None } else { Some(unquote(rest)) };
    lines.push(Line { indent, key, inline, lineno });
  }

  let mut idx = 0;
  let entries = parse_block(&lines, &mut idx, 0)?;
  if idx != lines.len() {
    return Err(format!("line {}: unexpected indentation", lines[idx].lineno));
  }
  Ok(entries)
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
  fn one_skill(extra_fields: &str) -> String {
    format!("skills:\n  s:\n{extra_fields}")
  }

  #[test]
  fn demo_config_is_valid() {
    let cfg = validate(demo_yaml()).expect("demo config should validate");
    assert_eq!(cfg.skills.len(), 2);
    // `add` runs by default and forwards A and B with injected defaults.
    let add = cfg.skills.iter().find(|s| s.name == "add").expect("add present");
    assert_eq!(add.harness, Harness::Opencode);
    assert_eq!(add.model.as_deref(), Some("openai/gpt-5.4-mini-fast"));
    assert_eq!(add.timeout, Some(600));
    assert_eq!(add.result, "tmp/add_result.json");
    assert_eq!(add.profile, None);
    assert!(add.commits, "add is commit-enabled (it commits the sum)");
    assert_eq!(
      add.env,
      vec![
        EnvVar { key: "A".into(), rule: EnvRule::Default { src: "A".into(), default: "2".into() } },
        EnvVar { key: "B".into(), rule: EnvRule::Default { src: "B".into(), default: "3".into() } },
      ]
    );
    // `multiply` is gated behind the `multiply` profile and requires X and Y (no defaults).
    let mul = cfg.skills.iter().find(|s| s.name == "multiply").expect("multiply present");
    assert_eq!(mul.profile.as_deref(), Some("multiply"));
    assert_eq!(mul.result, "tmp/multiply_result.json");
    assert!(!mul.commits, "multiply does not contribute commits");
    assert_eq!(
      mul.env,
      vec![
        EnvVar { key: "X".into(), rule: EnvRule::Require { src: "X".into(), message: required_message("X") } },
        EnvVar { key: "Y".into(), rule: EnvRule::Require { src: "Y".into(), message: required_message("Y") } },
      ]
    );
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
    let yes = one_skill("    harness: opencode\n    commits: true\n    result: tmp/x.json\n");
    assert!(validate(&yes).unwrap().skills[0].commits);
    let no = one_skill("    harness: opencode\n    commits: false\n    result: tmp/x.json\n");
    assert!(!validate(&no).unwrap().skills[0].commits);
    let default = one_skill("    harness: opencode\n    result: tmp/x.json\n");
    assert!(!validate(&default).unwrap().skills[0].commits, "commits defaults to false");
    let bad = one_skill("    harness: opencode\n    commits: yes\n    result: tmp/x.json\n");
    assert!(validate(&bad).unwrap_err().iter().any(|e| e.contains("'skills.s.commits' must be true or false")));
  }

  #[test]
  fn autoinstall_is_an_optional_boolean_defaulting_true() {
    let default = one_skill("    harness: opencode\n    result: tmp/x.json\n");
    assert!(validate(&default).unwrap().skills[0].autoinstall, "autoinstall defaults to true");
    let no = one_skill("    harness: opencode\n    autoinstall: false\n    result: tmp/x.json\n");
    assert!(!validate(&no).unwrap().skills[0].autoinstall);
    let yes = one_skill("    harness: opencode\n    autoinstall: true\n    result: tmp/x.json\n");
    assert!(validate(&yes).unwrap().skills[0].autoinstall);
    let bad = one_skill("    harness: opencode\n    autoinstall: nope\n    result: tmp/x.json\n");
    assert!(validate(&bad).unwrap_err().iter().any(|e| e.contains("'skills.s.autoinstall' must be true or false")));
  }

  #[test]
  fn extract_skill_block_keeps_fields_and_drops_autoinstall() {
    let yaml = "skills:\n  alpha:\n    autoinstall: false\n    harness: opencode\n    profile: rev\n    \
                result: tmp/a.json\n  beta:\n    harness: opencode\n    result: tmp/b.json\n";
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
  fn profile_is_an_optional_non_empty_string() {
    let with = one_skill("    harness: opencode\n    profile: full\n    result: tmp/x.json\n");
    assert_eq!(validate(&with).unwrap().skills[0].profile.as_deref(), Some("full"));
    let without = one_skill("    harness: opencode\n    result: tmp/x.json\n");
    assert_eq!(validate(&without).unwrap().skills[0].profile, None);
    let empty = one_skill("    harness: opencode\n    profile: \"\"\n    result: tmp/x.json\n");
    assert!(validate(&empty).unwrap_err().iter().any(|e| e.contains("'skills.s.profile' must not be empty")));
  }

  #[test]
  fn env_specs_parse_list_and_mapping_forms() {
    // List form (`- KEY: spec`): default-injecting, required (with and without a
    // custom message), bare $VAR, empty default, and a literal constant.
    let list = one_skill(
      "    harness: opencode\n    env:\n      - A: ${A:-5}\n      - X: ${X:?need X}\n      - R: ${R}\n      - S: $S\n      - E: ${E:-}\n      - LIT: hello\n    result: tmp/x.json\n",
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
    let map = one_skill("    harness: opencode\n    env:\n      A: ${A:-5}\n    result: tmp/x.json\n");
    assert_eq!(
      validate(&map).unwrap().skills[0].env,
      vec![EnvVar { key: "A".into(), rule: EnvRule::Default { src: "A".into(), default: "5".into() } }]
    );
  }

  #[test]
  fn bare_var_is_required_with_guidance_message() {
    // `${A}` and `$A` both require A and point at the `${A:-}` empty-default syntax.
    for spec in ["${A}", "$A"] {
      let src = one_skill(&format!("    harness: opencode\n    env:\n      - A: {spec}\n    result: tmp/x.json\n"));
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
    let with = one_skill("    harness: opencode\n    timeout: 30\n    result: tmp/x.json\n");
    assert_eq!(validate(&with).unwrap().skills[0].timeout, Some(30));
    let without = one_skill("    harness: opencode\n    result: tmp/x.json\n");
    assert_eq!(validate(&without).unwrap().skills[0].timeout, None);
    let zero = one_skill("    harness: opencode\n    timeout: 0\n    result: tmp/x.json\n");
    assert!(validate(&zero).unwrap_err().iter().any(|e| e.contains("positive number of seconds")));
    let bad = one_skill("    harness: opencode\n    timeout: soon\n    result: tmp/x.json\n");
    assert!(validate(&bad).unwrap_err().iter().any(|e| e.contains("integer number of seconds")));
  }

  #[test]
  fn env_rejects_bad_specs_names_and_dups() {
    // A malformed expansion (`:` followed by neither `-` nor `?`) is rejected.
    let bad_spec = one_skill("    harness: opencode\n    env:\n      - A: ${A:@x}\n    result: tmp/x.json\n");
    assert!(validate(&bad_spec).unwrap_err().iter().any(|e| e.contains("env.A")), "got {:?}", validate(&bad_spec));
    let bad_name = one_skill("    harness: opencode\n    env:\n      - 1BAD: ${X:-1}\n    result: tmp/x.json\n");
    assert!(validate(&bad_name).unwrap_err().iter().any(|e| e.contains("not a valid environment variable name")));
    let dup =
      one_skill("    harness: opencode\n    env:\n      - A: ${A:-1}\n      - A: ${A:-2}\n    result: tmp/x.json\n");
    assert!(validate(&dup).unwrap_err().iter().any(|e| e.contains("duplicate variable 'A'")));
  }

  #[test]
  fn parses_two_level_skills_in_file_order() {
    let src = "skills:\n  build:\n    harness: opencode\n    result: tmp/out.json\n  test:\n    harness: opencode\n    model: m\n    result: tmp/test.json\n";
    let cfg = validate(src).unwrap();
    assert_eq!(cfg.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["build", "test"]);
    assert_eq!(cfg.skills[0].model, None); // model is optional
    assert_eq!(cfg.skills[1].model.as_deref(), Some("m"));
  }

  #[test]
  fn result_is_required_per_skill() {
    let errs = validate(&one_skill("    harness: opencode\n")).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("missing required key 'result'")), "got {errs:?}");
  }

  #[test]
  fn harness_is_required_and_validated() {
    let missing = validate(&one_skill("    result: tmp/x.json\n")).unwrap_err();
    assert!(missing.iter().any(|e| e.contains("missing required key 'harness'")), "got {missing:?}");
    let unknown = validate(&one_skill("    harness: bogus\n    result: tmp/x.json\n")).unwrap_err();
    assert!(unknown.iter().any(|e| e.contains("not a known harness")), "got {unknown:?}");
  }

  #[test]
  fn result_rejects_paths_outside_the_repo() {
    for bad in ["/etc/passwd", "../escape.json", "a/../../b.json"] {
      let errs = validate(&one_skill(&format!("    harness: opencode\n    result: {bad}\n"))).unwrap_err();
      assert!(
        errs.iter().any(|e| e.contains("must be a path inside the repo")),
        "expected a result-path error for {bad}, got {errs:?}"
      );
    }
  }

  #[test]
  fn unknown_skill_key_reported() {
    let errs = validate(&one_skill("    harness: opencode\n    result: tmp/x.json\n    bogus: y\n")).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("unknown key 'skills.s.bogus'")), "got {errs:?}");
  }

  #[test]
  fn at_least_one_skill_required() {
    let errs = validate("skills:\n").unwrap_err();
    assert!(errs.iter().any(|e| e.contains("at least one skill")), "got {errs:?}");
  }

  #[test]
  fn duplicate_skill_reported() {
    let src = "skills:\n  s:\n    harness: opencode\n    result: tmp/a.json\n  s:\n    harness: opencode\n    result: tmp/b.json\n";
    let errs = validate(src).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("duplicate skill 's'")), "got {errs:?}");
  }

  #[test]
  fn strips_comments_full_line_and_trailing() {
    let src = "# a comment\nskills:\n  s:\n    harness: opencode   # trailing comment\n    result: tmp/x.json\n";
    let cfg = validate(src).unwrap();
    assert_eq!(cfg.skills.len(), 1);
    assert_eq!(cfg.skills[0].name, "s");
  }

  #[test]
  fn hash_inside_quotes_is_not_a_comment() {
    let src = one_skill("    harness: opencode\n    model: \"gpt#1\"\n    result: tmp/x.json\n");
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
  fn unknown_top_level_key_reported() {
    // version/project/image are no longer part of the schema — they read as unknown.
    let errs =
      validate(&format!("{}version: 1\n", one_skill("    harness: opencode\n    result: tmp/x.json\n"))).unwrap_err();
    assert!(errs.iter().any(|e| e.contains("unknown top-level key 'version'")), "got {errs:?}");
    assert!(errs.iter().any(|e| e.contains("only top-level key is 'skills'")), "got {errs:?}");
  }

  #[test]
  fn duplicate_top_level_key_reported() {
    let errs = validate(
      "skills:\n  s:\n    harness: opencode\n    result: tmp/a.json\nskills:\n  t:\n    harness: opencode\n    result: tmp/b.json\n",
    )
    .unwrap_err();
    assert!(errs.iter().any(|e| e.contains("duplicate top-level key 'skills'")), "got {errs:?}");
  }

  #[test]
  fn all_problems_collected_at_once() {
    // An unknown top-level key, a skill missing harness AND result, and an unknown
    // skill key — all reported in one pass.
    let errs = validate("nope: 1\nskills:\n  s:\n    bogus: y\n").unwrap_err();
    assert!(errs.len() >= 3, "expected several problems, got {errs:?}");
  }
}
