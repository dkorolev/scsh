//! Harness definitions — the `.harness/<name>.yml` runnable-job format.
//!
//! A *harness definition* is a parameterized job the daemon (or `scsh run --def`) can run:
//! a one-line `description`, typed `params` that render as a control form and are forwarded
//! to the container as environment variables, a `task` body that becomes the skill's
//! `SKILL.md`, and an `invocations:` agent matrix (the same schema as `.scsh.yml`).
//!
//! Terminology: the code elsewhere calls the AI CLI (claude/codex/opencode/grok/cursor) a
//! "harness" ([`crate::config::Harness`]). To avoid colliding with the user-facing name for
//! these definitions, new code here calls a `.harness/` entry a *harness definition*
//! ([`HarnessDef`]) and the CLI underneath it the definition's *agent*.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{self, EnvRule, EnvVar, InvocationRoute, Node, Skill};

/// Env override pointing directly at the user-global `.harness` directory. Tests set it so
/// discovery never reads the real home; power users may relocate their global definitions.
pub const HARNESS_HOME_ENV: &str = "SCSH_HARNESS_HOME";

/// The three built-in definitions, embedded at build time (mirrors `config::demo_yaml`), so
/// `doctor`/`add`/`research` are always available regardless of the repo. `(name, yaml)`.
pub fn builtin_defs() -> [(&'static str, &'static str); 3] {
  [
    ("doctor", include_str!("harness_defs/doctor.yml")),
    ("add", include_str!("harness_defs/add.yml")),
    ("research", include_str!("harness_defs/research.yml")),
  ]
}

/// Where a discovered definition came from — for the UI badge and discovery precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefSource {
  /// Embedded in the scsh binary ([`builtin_defs`]); lowest precedence.
  Builtin,
  /// A file under the running user's `~/.harness/`; overrides a built-in of the same name.
  Home,
  /// A file under the open repo's `.harness/`; overrides both home and built-in.
  Repo,
}

/// A parameter's value type. Determines the control the UI renders and how a supplied value
/// is validated before a run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
  /// Free text (rendered as a text input).
  String,
  /// An integer (rendered as a number input; validated with `i64::parse`).
  Int,
  /// `true`/`false` (rendered as a checkbox).
  Bool,
  /// One of a fixed set of `choices` (rendered as a select).
  Enum,
}

impl ParamType {
  fn parse(s: &str) -> Option<ParamType> {
    match s {
      "string" => Some(ParamType::String),
      "int" => Some(ParamType::Int),
      "bool" => Some(ParamType::Bool),
      "enum" => Some(ParamType::Enum),
      _ => None,
    }
  }
}

/// One declared parameter. Each becomes an environment variable of the same name forwarded
/// into the container (so `params` reuse the existing `${VAR:-default}` env machinery).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
  /// The variable name (also the env var); a valid POSIX-ish env name.
  pub name: String,
  /// The value type.
  pub ty: ParamType,
  /// The default value, if any. Presence makes the param optional.
  pub default: Option<String>,
  /// Whether a value must be supplied. Defaults to `true` unless a `default:` is given or
  /// `required: false` is set explicitly.
  pub required: bool,
  /// One-line human description shown as the form field's hint.
  pub description: Option<String>,
  /// Allowed values for an `enum` param (empty for other types).
  pub choices: Vec<String>,
}

impl Param {
  /// Build the [`EnvVar`] that forwards this param into the container:
  /// a `default:` param forwards the host value or injects the default; a required param
  /// with no default refuses the run when unset; an optional param with no default injects
  /// an empty value — exactly the `${VAR}` / `${VAR:-default}` semantics of `.scsh.yml`.
  pub fn to_env_var(&self) -> EnvVar {
    let src = self.name.clone();
    let rule = if let Some(default) = &self.default {
      EnvRule::Default { src, default: default.clone() }
    } else if self.required {
      EnvRule::Require { src, message: format!("harness-definition param '{}' is required", self.name) }
    } else {
      EnvRule::Default { src, default: String::new() }
    };
    EnvVar { key: self.name.clone(), rule }
  }

  /// Whether `value` is acceptable for this param's type. Used before a run starts (and by
  /// the UI). Returns a human-readable reason on rejection.
  pub fn validate_value(&self, value: &str) -> Result<(), String> {
    match self.ty {
      ParamType::String => Ok(()),
      ParamType::Int => value
        .trim()
        .parse::<i64>()
        .map(|_| ())
        .map_err(|_| format!("param '{}' must be an integer (got '{value}')", self.name)),
      ParamType::Bool => match value.trim() {
        "true" | "false" => Ok(()),
        other => Err(format!("param '{}' must be true or false (got '{other}')", self.name)),
      },
      ParamType::Enum => {
        if self.choices.iter().any(|c| c == value.trim()) {
          Ok(())
        } else {
          Err(format!("param '{}' must be one of: {} (got '{value}')", self.name, self.choices.join(", ")))
        }
      }
    }
  }
}

/// A parsed, validated harness definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessDef {
  /// The definition name (the `.harness/<name>.yml` file stem, or the built-in name).
  pub name: String,
  /// Where this definition was loaded from.
  pub source: DefSource,
  /// One-line description shown in the definition list.
  pub description: String,
  /// Declared parameters, in file order.
  pub params: Vec<Param>,
  /// The task prompt; materialized into the run clone as `.skills/<name>/SKILL.md`.
  pub task: String,
  /// The agent matrix — identical schema to a `.scsh.yml` skill's `invocations:`.
  pub invocations: Vec<InvocationRoute>,
}

impl HarnessDef {
  /// Compile this definition into a synthetic [`Skill`] so the existing run path
  /// (`expand_invocations` → `build_and_run`) runs it unchanged. Params become the skill's
  /// forwarded `env`; the agent matrix becomes its `invocations`; results land under `tmp/`.
  pub fn to_skill(&self) -> Skill {
    Skill {
      name: self.name.clone(),
      harness: None,
      model: None,
      effort: None,
      timeout: None,
      env: self.params.iter().map(Param::to_env_var).collect(),
      profile: None,
      commits: false,
      autoinstall: false,
      invocations: self.invocations.clone(),
      result: format!("tmp/{}_{{name}}.json", self.name),
    }
  }
}

/// The result of discovering the definitions available to a repo: the merged definitions
/// (built-in < home < repo precedence) plus any per-file parse warnings to surface.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
  /// Merged definitions, sorted by name.
  pub defs: Vec<HarnessDef>,
  /// One warning per `.harness/` file that failed to parse (`"<path>: <error>"`).
  pub warnings: Vec<String>,
}

impl Discovery {
  /// Find a definition by name.
  pub fn find(&self, name: &str) -> Option<&HarnessDef> {
    self.defs.iter().find(|d| d.name == name)
  }
}

/// Discover the definitions available to `repo_root`: the built-ins, overlaid by
/// `~/.harness/*.yml`, overlaid by `<repo_root>/.harness/*.yml`. Later sources shadow earlier
/// ones by name, so the effective precedence is repo > home > built-in.
pub fn discover(repo_root: &Path) -> Discovery {
  let mut map: BTreeMap<String, HarnessDef> = BTreeMap::new();
  let mut warnings = Vec::new();

  // Built-ins are embedded and covered by tests; a parse error here is a build-time bug, so
  // surface it as a warning rather than panicking a running daemon.
  for (name, src) in builtin_defs() {
    match validate(name, src, DefSource::Builtin) {
      Ok(def) => {
        map.insert(def.name.clone(), def);
      }
      Err(errs) => warnings.push(format!("built-in '{name}': {}", errs.join("; "))),
    }
  }

  if let Some(dir) = home_harness_dir() {
    load_dir(&dir, DefSource::Home, &mut map, &mut warnings);
  }
  load_dir(&repo_root.join(".harness"), DefSource::Repo, &mut map, &mut warnings);

  Discovery { defs: map.into_values().collect(), warnings }
}

/// The user-global `.harness` directory: `$SCSH_HARNESS_HOME`, else `$HOME/.harness`.
/// `None` when neither is set (headless with no home).
fn home_harness_dir() -> Option<PathBuf> {
  if let Some(dir) = std::env::var_os(HARNESS_HOME_ENV).filter(|s| !s.is_empty()) {
    return Some(PathBuf::from(dir));
  }
  std::env::var_os("HOME").filter(|s| !s.is_empty()).map(|home| PathBuf::from(home).join(".harness"))
}

/// Load every `*.yml` file in `dir` (if it exists) into `map`, keyed by file stem, replacing
/// any existing entry (so a later source shadows an earlier one). Files that fail to parse
/// add a warning and are skipped. Non-`.yml` entries and subdirectories are ignored.
fn load_dir(dir: &Path, source: DefSource, map: &mut BTreeMap<String, HarnessDef>, warnings: &mut Vec<String>) {
  let entries = match std::fs::read_dir(dir) {
    Ok(e) => e,
    Err(_) => return, // absent directory is normal, not an error
  };
  // Sort by path so discovery is deterministic regardless of readdir order.
  let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
  paths.sort();
  for path in paths {
    if path.extension().and_then(|e| e.to_str()) != Some("yml") {
      continue;
    }
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
    if !is_def_name(stem) {
      warnings.push(format!("{}: '{stem}' is not a valid definition name (use [A-Za-z0-9_-])", path.display()));
      continue;
    }
    let src = match std::fs::read_to_string(&path) {
      Ok(s) => s,
      Err(e) => {
        warnings.push(format!("{}: {e}", path.display()));
        continue;
      }
    };
    match validate(stem, &src, source) {
      Ok(def) => {
        map.insert(def.name.clone(), def);
      }
      Err(errs) => warnings.push(format!("{}: {}", path.display(), errs.join("; "))),
    }
  }
}

/// A definition name must be a safe single path component so it can key a `.skills/<name>/`
/// folder and a `tmp/<name>_*.json` result without escaping the repo.
fn is_def_name(s: &str) -> bool {
  !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Parse and validate one `.harness/<name>.yml` source, collecting every problem found (like
/// `config::validate`). `source` records where it came from for the UI.
pub fn validate(name: &str, src: &str, source: DefSource) -> Result<HarnessDef, Vec<String>> {
  let entries = match config::parse_yaml(src) {
    Ok(e) => e,
    Err(e) => return Err(vec![format!("invalid YAML: {e}")]),
  };

  let mut errors = Vec::new();
  let mut top: BTreeMap<&str, &Node> = BTreeMap::new();
  for (k, v) in &entries {
    if top.insert(k.as_str(), v).is_some() {
      errors.push(format!("duplicate top-level key '{k}'"));
    }
  }
  const KNOWN: &[&str] = &["description", "params", "task", "invocations"];
  for (k, _) in &entries {
    if !KNOWN.contains(&k.as_str()) {
      errors.push(format!("unknown top-level key '{k}' (allowed: description, params, task, invocations)"));
    }
  }

  let description = required_scalar(top.get("description").copied(), "description", &mut errors);
  let task = required_scalar(top.get("task").copied(), "task", &mut errors);

  let params = match top.get("params").copied() {
    None => Vec::new(),
    Some(Node::Scalar(_)) => {
      errors.push("'params' must be a mapping of named parameters".into());
      Vec::new()
    }
    Some(Node::Map(m)) => validate_params(m, &mut errors),
  };

  let invocations = match top.get("invocations").copied() {
    None => {
      errors.push("missing required key 'invocations' (an agent matrix, like a .scsh.yml skill)".into());
      Vec::new()
    }
    Some(node) => config::validate_invocations(name, node, &mut errors),
  };
  if top.contains_key("invocations") && invocations.is_empty() && errors.is_empty() {
    errors.push("'invocations' must list at least one agent route".into());
  }

  if errors.is_empty() {
    Ok(HarnessDef {
      name: name.to_string(),
      source,
      description: description.unwrap_or_default(),
      params,
      task: task.unwrap_or_default(),
      invocations,
    })
  } else {
    Err(errors)
  }
}

/// Read a required, non-empty scalar top-level field.
fn required_scalar(node: Option<&Node>, field: &str, errors: &mut Vec<String>) -> Option<String> {
  match node {
    None => {
      errors.push(format!("missing required key '{field}'"));
      None
    }
    Some(Node::Map(_)) => {
      errors.push(format!("'{field}' must be a string"));
      None
    }
    Some(Node::Scalar(s)) => {
      if s.trim().is_empty() {
        errors.push(format!("'{field}' must not be empty"));
        None
      } else {
        Some(s.clone())
      }
    }
  }
}

/// Validate the `params:` mapping into [`Param`]s, pushing every problem found.
fn validate_params(entries: &[(String, Node)], errors: &mut Vec<String>) -> Vec<Param> {
  let mut out = Vec::new();
  let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
  for (name, node) in entries {
    let name = name.trim();
    if !config::is_env_name(name) {
      errors.push(format!("param '{name}' is not a valid variable name ([A-Za-z_][A-Za-z0-9_]*)"));
      continue;
    }
    if seen.insert(name, ()).is_some() {
      errors.push(format!("duplicate param '{name}'"));
    }
    let fields = match node {
      Node::Map(f) => f,
      Node::Scalar(_) => {
        errors.push(format!("param '{name}' must be a mapping (type, default, required, description, choices)"));
        continue;
      }
    };
    let mut fm: BTreeMap<&str, &Node> = BTreeMap::new();
    for (k, v) in fields {
      if fm.insert(k.as_str(), v).is_some() {
        errors.push(format!("duplicate key 'params.{name}.{k}'"));
      }
    }
    const PK: &[&str] = &["type", "default", "required", "description", "choices"];
    for (k, _) in fields {
      if !PK.contains(&k.as_str()) {
        errors
          .push(format!("unknown key 'params.{name}.{k}' (allowed: type, default, required, description, choices)"));
      }
    }

    let ty = match fm.get("type").copied() {
      None => ParamType::String, // default type
      Some(Node::Scalar(s)) => match ParamType::parse(s.trim()) {
        Some(t) => t,
        None => {
          errors.push(format!("'params.{name}.type' is '{}', not one of: string, int, bool, enum", s.trim()));
          ParamType::String
        }
      },
      Some(Node::Map(_)) => {
        errors.push(format!("'params.{name}.type' must be a string"));
        ParamType::String
      }
    };

    let default = opt_scalar(&fm, name, "default", errors);
    let description = opt_scalar(&fm, name, "description", errors);

    let choices = match fm.get("choices").copied() {
      None => Vec::new(),
      Some(Node::Scalar(s)) => s.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
      Some(Node::Map(_)) => {
        errors.push(format!("'params.{name}.choices' must be a comma-separated string (e.g. \"a, b, c\")"));
        Vec::new()
      }
    };
    if ty == ParamType::Enum && choices.is_empty() {
      errors.push(format!("'params.{name}' is an enum but has no 'choices'"));
    }
    if ty != ParamType::Enum && !choices.is_empty() {
      errors.push(format!("'params.{name}.choices' is only valid for an enum param"));
    }

    // required defaults to true, unless a default is given or required:false is explicit.
    let required = match fm.get("required").copied() {
      None => default.is_none(),
      Some(Node::Scalar(s)) => match s.trim() {
        "true" => true,
        "false" => false,
        other => {
          errors.push(format!("'params.{name}.required' must be true or false (got '{other}')"));
          default.is_none()
        }
      },
      Some(Node::Map(_)) => {
        errors.push(format!("'params.{name}.required' must be true or false"));
        default.is_none()
      }
    };

    let param = Param { name: name.to_string(), ty, default, required, description, choices };
    // A supplied default must itself satisfy the declared type/choices.
    if let Some(d) = &param.default {
      if let Err(e) = param.validate_value(d) {
        errors.push(format!("'params.{name}.default' is invalid: {e}"));
      }
    }
    out.push(param);
  }
  out
}

/// Read an optional non-empty scalar sub-field of a param.
fn opt_scalar(fm: &BTreeMap<&str, &Node>, param: &str, field: &str, errors: &mut Vec<String>) -> Option<String> {
  match fm.get(field).copied() {
    None => None,
    Some(Node::Scalar(s)) => Some(s.clone()),
    Some(Node::Map(_)) => {
      errors.push(format!("'params.{param}.{field}' must be a string"));
      None
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Index into `builtin_defs()` for readability.
  fn builtin(name: &str) -> HarnessDef {
    let (_, src) = builtin_defs().into_iter().find(|(n, _)| *n == name).expect("known built-in");
    validate(name, src, DefSource::Builtin).unwrap_or_else(|e| panic!("{name}: {}", e.join("; ")))
  }

  #[test]
  fn builtins_parse_and_have_expected_shape() {
    let add = builtin("add");
    assert_eq!(add.params.len(), 2);
    assert!(add.params.iter().all(|p| p.ty == ParamType::Int));
    assert_eq!(add.params.iter().find(|p| p.name == "A").unwrap().default.as_deref(), Some("2"));
    // The `task:` block scalar is preserved verbatim across multiple lines.
    assert!(add.task.contains('\n'), "task should be multi-line");
    assert!(add.task.contains("SCSH_RESULT"), "task body preserved");
    assert_eq!(add.invocations.len(), 2);

    let research = builtin("research");
    let city = research.params.iter().find(|p| p.name == "CITY").unwrap();
    assert!(city.required && city.default.is_none(), "CITY is required with no default");
    let area = research.params.iter().find(|p| p.name == "AREA").unwrap();
    assert!(!area.required && area.default.as_deref() == Some(""), "AREA optional, empty default");

    let doctor = builtin("doctor");
    assert!(doctor.params.is_empty());
    assert_eq!(doctor.invocations.len(), 3);
  }

  #[test]
  fn params_compile_to_env_rules() {
    let with_default = Param {
      name: "A".into(),
      ty: ParamType::Int,
      default: Some("2".into()),
      required: false,
      description: None,
      choices: vec![],
    };
    match with_default.to_env_var().rule {
      EnvRule::Default { src, default } => {
        assert_eq!(src, "A");
        assert_eq!(default, "2");
      }
      other => panic!("expected Default, got {other:?}"),
    }

    let required = Param {
      name: "CITY".into(),
      ty: ParamType::String,
      default: None,
      required: true,
      description: None,
      choices: vec![],
    };
    assert!(matches!(required.to_env_var().rule, EnvRule::Require { .. }));

    let optional = Param {
      name: "AREA".into(),
      ty: ParamType::String,
      default: None,
      required: false,
      description: None,
      choices: vec![],
    };
    match optional.to_env_var().rule {
      EnvRule::Default { default, .. } => assert_eq!(default, ""),
      other => panic!("expected empty Default, got {other:?}"),
    }
  }

  #[test]
  fn value_validation_by_type() {
    let int =
      Param { name: "N".into(), ty: ParamType::Int, default: None, required: true, description: None, choices: vec![] };
    assert!(int.validate_value("42").is_ok());
    assert!(int.validate_value("x").is_err());

    let boolean = Param {
      name: "B".into(),
      ty: ParamType::Bool,
      default: None,
      required: true,
      description: None,
      choices: vec![],
    };
    assert!(boolean.validate_value("true").is_ok());
    assert!(boolean.validate_value("yes").is_err());

    let choice = Param {
      name: "E".into(),
      ty: ParamType::Enum,
      default: None,
      required: true,
      description: None,
      choices: vec!["a".into(), "b".into()],
    };
    assert!(choice.validate_value("a").is_ok());
    assert!(choice.validate_value("c").is_err());
  }

  #[test]
  fn compiles_to_skill_and_expands() {
    let skill = builtin("add").to_skill();
    assert_eq!(skill.name, "add");
    assert!(skill.harness.is_none());
    assert_eq!(skill.env.len(), 2);
    assert!(skill.result.contains("{name}"));

    let cfg = crate::config::Config { skills: vec![skill], terminal: crate::config::Terminal::default() };
    let inv = crate::config::expand_invocations(&cfg);
    assert_eq!(inv.len(), 2);
    assert!(inv.iter().any(|i| i.name == "add-claude-sonnet-4-6"));
    // Each route substitutes its own name into the result path (no collisions).
    assert!(inv.iter().any(|i| i.result == "tmp/add_claude-sonnet-4-6.json"));
  }

  #[test]
  fn unknown_and_missing_keys_are_rejected() {
    let bad =
      "description: \"x\"\ntask: |\n  go\ninvocations:\n  c:\n    harness: claude\n    model: sonnet\nbogus: 1\n";
    let err = validate("t", bad, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("bogus")), "{err:?}");

    let no_task = "description: \"x\"\ninvocations:\n  c:\n    harness: claude\n    model: sonnet\n";
    let err = validate("t", no_task, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("task")), "{err:?}");
  }

  #[test]
  fn repo_shadows_builtin_by_name() {
    let mut map: BTreeMap<String, HarnessDef> = BTreeMap::new();
    let add = builtin("add");
    map.insert(add.name.clone(), add);
    assert_eq!(map["add"].source, DefSource::Builtin);

    let base = std::env::temp_dir().join(format!("scsh-hd-{}", crate::runtime::random_nonce_6()));
    let hdir = base.join(".harness");
    std::fs::create_dir_all(&hdir).unwrap();
    std::fs::write(
      hdir.join("add.yml"),
      "description: \"Repo add.\"\ntask: |\n  do it\ninvocations:\n  c:\n    harness: claude\n    model: sonnet\n",
    )
    .unwrap();

    let mut warnings = Vec::new();
    load_dir(&hdir, DefSource::Repo, &mut map, &mut warnings);
    assert!(warnings.is_empty(), "warnings: {warnings:?}");
    assert_eq!(map["add"].source, DefSource::Repo);
    assert_eq!(map["add"].description, "Repo add.");
    std::fs::remove_dir_all(&base).ok();
  }

  #[test]
  fn discover_merges_builtins_home_and_repo() {
    let base = std::env::temp_dir().join(format!("scsh-disc-{}", crate::runtime::random_nonce_6()));
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap(); // empty home .harness
    let repo = base.join("repo");
    let rh = repo.join(".harness");
    std::fs::create_dir_all(&rh).unwrap();
    std::fs::write(
      rh.join("mine.yml"),
      "description: \"Mine.\"\ntask: |\n  go\ninvocations:\n  c:\n    harness: claude\n    model: sonnet\n",
    )
    .unwrap();

    std::env::set_var(HARNESS_HOME_ENV, &home);
    let d = discover(&repo);
    std::env::remove_var(HARNESS_HOME_ENV);

    assert!(d.find("doctor").is_some() && d.find("add").is_some() && d.find("research").is_some());
    let mine = d.find("mine").expect("repo def discovered");
    assert_eq!(mine.source, DefSource::Repo);
    assert!(d.warnings.is_empty(), "warnings: {:?}", d.warnings);
    std::fs::remove_dir_all(&base).ok();
  }
}
