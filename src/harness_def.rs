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

/// The built-in definitions, embedded at build time (mirrors `config::demo_yaml`), so
/// `doctor`/`add`/`research`/`demo-pr`/`smoke-pr-*` (flat) and `fruits`/`code-review`/`arith`/`greet`
/// (workflows) are always available regardless of the repo. `(name, yaml)`.
pub fn builtin_defs() -> [(&'static str, &'static str); 18] {
  [
    ("doctor", include_str!("harness_defs/doctor.yml")),
    ("add", include_str!("harness_defs/add.yml")),
    ("research", include_str!("harness_defs/research.yml")),
    ("fruits", include_str!("harness_defs/fruits.yml")),
    ("code-review", include_str!("harness_defs/code-review.yml")),
    ("arith", include_str!("harness_defs/arith.yml")),
    ("greet", include_str!("harness_defs/greet.yml")),
    ("demo-pr", include_str!("harness_defs/demo-pr.yml")),
    ("demo-loop-repeat", include_str!("harness_defs/demo-loop-repeat.yml")),
    ("demo-loop-do-while", include_str!("harness_defs/demo-loop-do-while.yml")),
    ("demo-loop-break", include_str!("harness_defs/demo-loop-break.yml")),
    ("demo-beautiful-loop", include_str!("harness_defs/demo-beautiful-loop.yml")),
    ("gorgeous-pipeline", include_str!("harness_defs/gorgeous-pipeline.yml")),
    ("big-beautiful-build", include_str!("harness_defs/big-beautiful-build.yml")),
    ("smoke-pr-claude", include_str!("harness_defs/smoke-pr-claude.yml")),
    ("smoke-pr-codex", include_str!("harness_defs/smoke-pr-codex.yml")),
    ("smoke-pr-grok", include_str!("harness_defs/smoke-pr-grok.yml")),
    ("smoke-pr-cursor", include_str!("harness_defs/smoke-pr-cursor.yml")),
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

impl DefSource {
  /// A stable lowercase tag for JSON/UI (`"builtin"`, `"home"`, `"repo"`).
  pub fn as_str(self) -> &'static str {
    match self {
      DefSource::Builtin => "builtin",
      DefSource::Home => "home",
      DefSource::Repo => "repo",
    }
  }
}

/// A parameter's value type. Determines the control the UI renders and how a supplied value
/// is validated before a run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
  /// Free text (rendered as a text input).
  String,
  /// Free-form, non-empty prose (rendered as a multiline text area).
  Text,
  /// An integer (rendered as a number input; validated with `i64::parse`).
  Int,
  /// `true`/`false` (rendered as a checkbox).
  Bool,
  /// One of a fixed set of `choices` (rendered as a select).
  Enum,
}

/// A workflow result field's machine type. Output values cross a JSON boundary, so this is
/// deliberately distinct from [`ParamType`]: arrays are valid workflow results but are not
/// scalar HTML-form parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputType {
  /// A JSON string.
  String,
  /// An integral JSON number.
  Int,
  /// A JSON boolean.
  Bool,
  /// A JSON string restricted to one of the field's declared choices.
  Enum,
  /// A JSON array containing only strings.
  StringList,
}

impl OutputType {
  fn parse(s: &str) -> Option<OutputType> {
    match s {
      "string" => Some(OutputType::String),
      "int" => Some(OutputType::Int),
      "bool" => Some(OutputType::Bool),
      "enum" => Some(OutputType::Enum),
      "string_list" => Some(OutputType::StringList),
      _ => None,
    }
  }

  /// The human-readable type label rendered into the workflow step's output contract.
  pub fn as_str(self) -> &'static str {
    match self {
      OutputType::String => "string",
      OutputType::Int => "int",
      OutputType::Bool => "bool",
      OutputType::Enum => "enum",
      OutputType::StringList => "array of strings",
    }
  }
}

impl ParamType {
  fn parse(s: &str) -> Option<ParamType> {
    match s {
      "string" => Some(ParamType::String),
      "text" => Some(ParamType::Text),
      "int" => Some(ParamType::Int),
      "bool" => Some(ParamType::Bool),
      "enum" => Some(ParamType::Enum),
      _ => None,
    }
  }

  /// A stable lowercase tag for JSON/UI (`"string"`, `"int"`, `"bool"`, `"enum"`).
  pub fn as_str(self) -> &'static str {
    match self {
      ParamType::String => "string",
      ParamType::Text => "text",
      ParamType::Int => "int",
      ParamType::Bool => "bool",
      ParamType::Enum => "enum",
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
      ParamType::Text if self.required && value.trim().is_empty() => {
        Err(format!("param '{}' must not be empty", self.name))
      }
      ParamType::Text => Ok(()),
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

/// A reference in a `when:` condition or `inputs:` binding — either a run parameter
/// (`params.NAME`) or a field of an upstream step's validated output (`stepid.field`). This is
/// the ONE reference form workflows use; there is no expression language, only these two shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
  /// `params.NAME` — a run parameter.
  Param(String),
  /// `stepid.field` — a field of an upstream step's `output`.
  StepField { step: String, field: String },
}

impl Ref {
  /// Parse a `head.tail` reference. `params.NAME` is a param; anything else is `stepid.field`.
  fn parse(s: &str) -> Option<Ref> {
    let (head, tail) = s.trim().split_once('.')?;
    let (head, tail) = (head.trim(), tail.trim());
    if head.is_empty() || tail.is_empty() || tail.contains('.') {
      return None;
    }
    if head == "params" {
      Some(Ref::Param(tail.to_string()))
    } else {
      Some(Ref::StepField { step: head.to_string(), field: tail.to_string() })
    }
  }
}

/// One `inputs:` binding: the env var name the step receives (its own name), bound to a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputBinding {
  /// The environment variable the running step sees.
  pub name: String,
  /// Where its value comes from (a param or an upstream step's output field).
  pub source: Ref,
}

/// A comparison operator in a `when:` condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondOp {
  Eq,
  Ne,
  Lt,
  Lte,
  Gt,
  Gte,
  In,
}

impl CondOp {
  fn parse(s: &str) -> Option<CondOp> {
    match s {
      "eq" => Some(CondOp::Eq),
      "ne" => Some(CondOp::Ne),
      "lt" => Some(CondOp::Lt),
      "lte" => Some(CondOp::Lte),
      "gt" => Some(CondOp::Gt),
      "gte" => Some(CondOp::Gte),
      "in" => Some(CondOp::In),
      _ => None,
    }
  }
}

/// One condition: a reference compared against a literal (a comma-separated list, for `in`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cond {
  pub reference: Ref,
  pub op: CondOp,
  /// The comparison value(s) — one, except `in` which takes several.
  pub values: Vec<String>,
}

/// A step gate: a set of conditions, ALL of which must hold (AND). Disjunction ("run in either
/// case") is expressed as separate steps, so the format needs no OR combinator — which also
/// keeps `when:` a plain block map the minimal YAML reader can parse.
pub type When = Vec<Cond>;

/// One output field a step promises to write to its result JSON (name + type, enum choices).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputField {
  /// Top-level JSON field name.
  pub name: String,
  /// Exact JSON type accepted for this field.
  pub ty: OutputType,
  /// Allowed string values when `ty` is [`OutputType::Enum`]; empty otherwise.
  pub choices: Vec<String>,
}

/// The agent (CLI + model) that runs a single step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAgent {
  pub harness: crate::config::Harness,
  pub model: Option<String>,
  pub effort: Option<String>,
}

/// The authored work for a workflow step. Inline prompts suit small jobs; a bundled skill keeps
/// a substantial reusable contract in its canonical `.skills/<name>/SKILL.md` source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepTask {
  /// Prose written directly in the harness definition's `prompt:` block.
  Prompt(String),
  /// A named skill from [`config::bundled_skills`], resolved while the definition is parsed.
  BundledSkill {
    /// Stable skill name shown in the definition and errors.
    name: String,
    /// Canonical embedded `SKILL.md` body delivered to the agent.
    body: String,
  },
}

impl StepTask {
  /// The exact prose delivered before `scsh` appends the workflow I/O contract.
  pub fn body(&self) -> &str {
    match self {
      StepTask::Prompt(body) | StepTask::BundledSkill { body, .. } => body,
    }
  }
}

/// One node in a workflow DAG. A step is a context-free unit: it receives its `inputs` as
/// named environment variables and writes its `output` fields to `$SCSH_RESULT` — it knows
/// nothing about the graph, other steps, or its own position (scsh resolves all of that).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
  /// Unique step id (`[A-Za-z0-9_]`).
  pub id: String,
  /// The agent that runs this step.
  pub agent: StepAgent,
  /// The task prompt or canonical bundled skill; scsh appends the I/O contract.
  pub task: StepTask,
  /// Input bindings: each names an env var the step sees and where its value comes from.
  pub inputs: Vec<InputBinding>,
  /// The typed result fields this step must produce (validated against `$SCSH_RESULT`).
  pub outputs: Vec<OutputField>,
  /// Optional gate: the step runs only when this evaluates true.
  pub when: Option<When>,
  /// Steps that must finish (or be skipped) before this one — the DAG edges.
  pub needs: Vec<String>,
  /// Extra files the step must write NEXT TO its `$SCSH_RESULT` (plain filenames, no
  /// directories) — copied back into the caller repo's session dir exactly like the result,
  /// and required once declared. For deliverables that are files, not JSON fields (e.g. a
  /// plain-English `summary.txt`).
  pub artifacts: Vec<String>,
  /// When true, commits the step makes inside the clone are rebased onto the caller's
  /// branch (and packed with packdiff when available) — same contract as a skill's
  /// `commits: true`.
  pub commits: bool,
  /// Run this step a fixed number of times, sequentially. Each iteration is a distinct
  /// workflow run and commit boundary; the graph discovers iterations as they start.
  pub repeat: Option<usize>,
  /// Mark this as the final step of a do-while body and name that body's first step. The final
  /// step's result JSON decides whether to repeat via the fixed top-level boolean
  /// `SCSH_DO_WHILE_REPEAT`; scsh deliberately has no built-in comparison language.
  pub do_while: Option<String>,
  /// This step is the first step of a do-while body and may end that loop immediately by
  /// returning the fixed top-level boolean `SCSH_LOOP_BREAK`.
  pub break_loop: bool,
}

/// Hard backstop for `do-while` loops — each iteration is a full agent run, so a condition
/// that never flips must fail the workflow rather than loop indefinitely. Far above any loop
/// a definition should author; not a tuning knob.
pub const DO_WHILE_MAX_ITERATIONS: usize = 25;

/// A parsed, validated harness definition — either a flat one-shot task or a workflow of steps.
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
  /// Flat form: the task prompt (`None` for a workflow), materialized into the run clone as
  /// `.skills/<name>/SKILL.md`.
  pub task: Option<String>,
  /// Flat form: the agent matrix — identical schema to a `.scsh.yml` skill's `invocations:`
  /// (empty for a workflow).
  pub invocations: Vec<InvocationRoute>,
  /// Workflow form: the DAG of steps (empty for a flat definition).
  pub steps: Vec<Step>,
}

impl HarnessDef {
  /// Whether this is a workflow (has `steps:`) rather than a flat one-shot task.
  pub fn is_workflow(&self) -> bool {
    !self.steps.is_empty()
  }

  /// Compile a FLAT definition into a synthetic [`Skill`] so the existing run path
  /// (`expand_invocations` → `build_and_run`) runs it unchanged. Params become the skill's
  /// forwarded `env`; the agent matrix becomes its `invocations`; results land under `tmp/`.
  /// (Workflows do not use this; the orchestrator builds a per-step invocation instead.)
  pub fn to_skill(&self) -> Skill {
    Skill {
      name: self.name.clone(),
      harness: None,
      model: None,
      effort: None,
      timeout: None,
      inactivity_timeout: None,
      env: self.params.iter().map(Param::to_env_var).collect(),
      profile: None,
      commits: false,
      autoinstall: false,
      invocations: self.invocations.clone(),
      result: format!("tmp/{}_{{name}}.json", self.name),
    }
  }
}

impl Step {
  /// Whether this step is a loop (`repeat` or `do-while`) — its iterations run sequentially as
  /// distinct workflow runs, discovered by the graph only as they start.
  pub fn is_loop(&self) -> bool {
    self.repeat.is_some() || self.do_while.is_some()
  }

  /// The run id of one loop iteration — `<id>-repeat-<n>` / `<id>-while-<end>-<n>` (the graph
  /// parses this shape back into "step · iteration n"; dashes cannot appear in step ids, so it
  /// is unambiguous and reads cleanly in file names) — or the plain step id when not a loop.
  pub fn iteration_run_id(&self, iteration: usize) -> String {
    if self.repeat.is_some() {
      format!("{}-repeat-{iteration}", self.id)
    } else if self.do_while.is_some() {
      format!("{}-while-{}-{iteration}", self.id, self.id)
    } else {
      self.id.clone()
    }
  }

  /// The human word for this step's loop kind ("repeat" / "do-while"), for labels and notes.
  pub fn loop_kind(&self) -> &'static str {
    if self.repeat.is_some() {
      "repeat"
    } else {
      "do-while"
    }
  }

  /// The full prompt scsh sends to the harness for this step: the author's `prompt` plus the
  /// scsh-generated I/O contract — which env vars carry the inputs, and the exact JSON shape to
  /// write to `$SCSH_RESULT`. The author writes intent; scsh guarantees the machine contract.
  /// Delivered as a harness custom prompt ([`crate::config::SkillDelivery::DirectPrompt`]), not
  /// as a synthetic `SKILL.md`.
  pub fn render_skill_body(&self) -> String {
    let mut s = self.task.body().trim_end().to_string();
    s.push_str("\n\n## Inputs\n\n");
    if self.inputs.is_empty() {
      s.push_str("This step takes no inputs.\n");
    } else {
      s.push_str("These values are provided as environment variables:\n");
      for b in &self.inputs {
        s.push_str(&format!("- `{}`\n", b.name));
      }
    }
    s.push_str("\n## Output\n\nWrite a single JSON object to the file at `$SCSH_RESULT` with exactly these fields:\n");
    for o in &self.outputs {
      let ty = match o.ty {
        OutputType::Enum => format!("one of: {}", o.choices.join(", ")),
        other => other.as_str().to_string(),
      };
      s.push_str(&format!("- `{}` ({ty})\n", o.name));
    }
    if self.do_while.is_some() && !self.outputs.iter().any(|o| o.name == "SCSH_DO_WHILE_REPEAT") {
      s.push_str("- `SCSH_DO_WHILE_REPEAT` (boolean; `true` requests another loop iteration, `false` ends the loop)\n");
    }
    if self.break_loop {
      s.push_str(
        "\n`SCSH_LOOP_BREAK: true` exits this do-while immediately; `false` continues with the rest of the body.\n",
      );
    }
    s.push_str("\nDo not write anything else to that file.\n");
    if !self.artifacts.is_empty() {
      s.push_str("\n## Required files\n\nAlso write, in the SAME directory as the `$SCSH_RESULT` file:\n");
      for a in &self.artifacts {
        s.push_str(&format!("- `{a}`\n"));
      }
      s.push_str("\nThese files are required; the step fails without them.\n");
    }
    if self.commits {
      s.push_str(
        "\n## Commits\n\nThis step is commit-enabled: any `git commit` you make in the repo \
         is brought back onto the caller's branch after the step finishes. Commit only the \
         files this step is meant to change — never anything under `tmp/`.\n",
      );
    }
    s
  }
}

impl Cond {
  /// Evaluate this condition. `value_of` returns the current string value of a reference
  /// (a param value, or a field of an upstream step's result), or `None` if unavailable.
  pub fn eval(&self, value_of: &impl Fn(&Ref) -> Option<String>) -> bool {
    let Some(actual) = value_of(&self.reference) else { return false };
    match self.op {
      CondOp::Eq => self.values.first().is_some_and(|v| *v == actual),
      CondOp::Ne => self.values.first().is_some_and(|v| *v != actual),
      CondOp::In => self.values.contains(&actual),
      _ => {
        let (Ok(a), Some(Ok(b))) = (actual.trim().parse::<i64>(), self.values.first().map(|v| v.trim().parse::<i64>()))
        else {
          return false;
        };
        match self.op {
          CondOp::Lt => a < b,
          CondOp::Lte => a <= b,
          CondOp::Gt => a > b,
          CondOp::Gte => a >= b,
          _ => unreachable!("non-ordering op handled above"),
        }
      }
    }
  }
}

/// Whether a step's `when:` gate holds — every condition must (they are AND-ed).
pub fn when_holds(when: &When, value_of: &impl Fn(&Ref) -> Option<String>) -> bool {
  when.iter().all(|c| c.eval(value_of))
}

fn format_ref(r: &Ref) -> String {
  match r {
    Ref::Param(name) => format!("params.{name}"),
    Ref::StepField { step, field } => format!("{step}.{field}"),
  }
}

#[allow(dead_code)] // kept for tests / future UI that wants a human gate phrase offline
fn format_cond(c: &Cond) -> String {
  let lhs = format_ref(&c.reference);
  match c.op {
    CondOp::Eq => format!("{lhs} = {}", c.values.first().map(String::as_str).unwrap_or("")),
    CondOp::Ne => format!("{lhs} ≠ {}", c.values.first().map(String::as_str).unwrap_or("")),
    CondOp::Lt => format!("{lhs} < {}", c.values.first().map(String::as_str).unwrap_or("")),
    CondOp::Lte => format!("{lhs} ≤ {}", c.values.first().map(String::as_str).unwrap_or("")),
    CondOp::Gt => format!("{lhs} > {}", c.values.first().map(String::as_str).unwrap_or("")),
    CondOp::Gte => format!("{lhs} ≥ {}", c.values.first().map(String::as_str).unwrap_or("")),
    CondOp::In => format!("{lhs} in [{}]", c.values.join(", ")),
  }
}

/// One-line human summary of a `when:` gate for UI tooltips (AND of every condition).
#[allow(dead_code)] // privacy: not persisted on WorkflowMeta; still unit-tested
pub fn format_when_summary(when: &When) -> String {
  let body = when.iter().map(format_cond).collect::<Vec<_>>().join(" and ");
  format!("Runs only if {body}")
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
  const KNOWN: &[&str] = &["description", "params", "task", "invocations", "steps"];
  for (k, _) in &entries {
    if !KNOWN.contains(&k.as_str()) {
      errors.push(format!("unknown top-level key '{k}' (allowed: description, params, task, invocations, steps)"));
    }
  }

  let description = required_scalar(top.get("description").copied(), "description", &mut errors);

  let params = match top.get("params").copied() {
    None => Vec::new(),
    Some(Node::Scalar(_)) => {
      errors.push("'params' must be a mapping of named parameters".into());
      Vec::new()
    }
    Some(Node::Map(m)) => validate_params(m, &mut errors),
  };

  // A definition is EITHER a workflow (`steps:`) OR a flat one-shot task (`task:`+`invocations:`).
  let stepped = top.contains_key("steps");
  let flat = top.contains_key("task") || top.contains_key("invocations");
  let mut task = None;
  let mut invocations = Vec::new();
  let mut steps = Vec::new();
  if stepped && flat {
    errors
      .push("a definition uses either 'steps:' (a workflow) or 'task:'+'invocations:' (a one-shot), not both".into());
  } else if stepped {
    steps = validate_steps(top.get("steps").copied(), &params, &mut errors);
  } else {
    task = required_scalar(top.get("task").copied(), "task", &mut errors);
    invocations = match top.get("invocations").copied() {
      None => {
        errors.push("missing required key 'invocations' (an agent matrix, like a .scsh.yml skill) — or use 'steps:' for a workflow".into());
        Vec::new()
      }
      Some(node) => config::validate_invocations(name, node, &mut errors),
    };
    if top.contains_key("invocations") && invocations.is_empty() && errors.is_empty() {
      errors.push("'invocations' must list at least one agent route".into());
    }
  }

  if errors.is_empty() {
    Ok(HarnessDef {
      name: name.to_string(),
      source,
      description: description.unwrap_or_default(),
      params,
      task,
      invocations,
      steps,
    })
  } else {
    Err(errors)
  }
}

/// Validate the `steps:` block map (keyed by step id) into a DAG: each step has an agent, a
/// prompt or bundled skill, and typed `output` fields; `inputs`/`when` references resolve to a declared param or
/// an upstream step's output field; `needs` names other steps; and the graph is acyclic. The
/// minimal YAML reader has no flow collections, so `steps:` is a block map (not a sequence),
/// `needs:` is a comma-separated scalar, and `when:` is a plain block map (AND of its entries).
fn validate_steps(node: Option<&Node>, params: &[Param], errors: &mut Vec<String>) -> Vec<Step> {
  let entries = match node {
    Some(Node::Map(m)) if !m.is_empty() => m,
    Some(Node::Map(_)) => {
      errors.push("'steps' must define at least one step".into());
      return Vec::new();
    }
    _ => {
      errors.push("'steps' must be a mapping of named steps".into());
      return Vec::new();
    }
  };

  let mut steps: Vec<Step> = Vec::new();
  let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
  for (id, node) in entries {
    let id = id.trim();
    if !config::is_env_name(id) {
      errors.push(format!("step id '{id}' is not a valid identifier ([A-Za-z_][A-Za-z0-9_]*)"));
      continue;
    }
    if seen.insert(id, ()).is_some() {
      errors.push(format!("duplicate step '{id}'"));
    }
    let fields = match node {
      Node::Map(f) => f,
      Node::Scalar(_) => {
        errors.push(format!(
          "step '{id}' must be a mapping (agent, prompt, inputs, output, when, needs, artifacts, commits)"
        ));
        continue;
      }
    };
    let mut fm: BTreeMap<&str, &Node> = BTreeMap::new();
    for (k, v) in fields {
      if fm.insert(k.as_str(), v).is_some() {
        errors.push(format!("duplicate key 'steps.{id}.{k}'"));
      }
    }
    const SK: &[&str] = &[
      "agent",
      "prompt",
      "skill",
      "inputs",
      "output",
      "when",
      "needs",
      "artifacts",
      "commits",
      "repeat",
      "do-while",
      "break",
    ];
    for (k, _) in fields {
      if !SK.contains(&k.as_str()) {
        errors.push(format!(
          "unknown key 'steps.{id}.{k}' (allowed: agent, prompt, skill, inputs, output, when, needs, artifacts, commits, repeat, do-while, break)"
        ));
      }
    }

    let agent = validate_step_agent(id, fm.get("agent").copied(), errors);
    let task = validate_step_task(id, fm.get("prompt").copied(), fm.get("skill").copied(), errors);
    let inputs = validate_step_inputs(id, fm.get("inputs").copied(), errors);
    let outputs = validate_step_outputs(id, fm.get("output").copied(), errors);
    let when = validate_step_cond_block(id, "when", fm.get("when").copied(), errors);
    let do_while = match fm.get("do-while") {
      None => None,
      Some(Node::Scalar(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
      Some(Node::Scalar(_)) => {
        errors.push(format!("'steps.{id}.do-while' must name the first step of the loop body"));
        None
      }
      Some(_) => {
        errors.push(format!(
          "'steps.{id}.do-while' must be a step name, not a comparator block; the final step must return SCSH_DO_WHILE_REPEAT"
        ));
        None
      }
    };
    let needs = parse_needs(fm.get("needs").copied());
    let artifacts = parse_needs(fm.get("artifacts").copied());
    for a in &artifacts {
      // Artifacts land beside the step's result inside the session scratch dir; a plain
      // filename is the whole contract — no directories, no traversal.
      if a.contains('/') || a.contains("..") {
        errors.push(format!("'steps.{id}.artifacts': '{a}' must be a plain filename (no '/' or '..')"));
      }
    }
    let commits = match fm.get("commits") {
      None => false,
      Some(Node::Scalar(s)) => match s.trim() {
        "true" | "yes" | "on" | "1" => true,
        "false" | "no" | "off" | "0" => false,
        other => {
          errors.push(format!("'steps.{id}.commits': expected a boolean, got '{other}'"));
          false
        }
      },
      Some(_) => {
        errors.push(format!("'steps.{id}.commits': expected a boolean"));
        false
      }
    };
    let repeat = match fm.get("repeat") {
      None => None,
      Some(Node::Scalar(s)) => match s.trim().parse::<usize>() {
        Ok(n) if n > 0 => Some(n),
        _ => {
          errors.push(format!("'steps.{id}.repeat' must be a positive integer"));
          None
        }
      },
      Some(_) => {
        errors.push(format!("'steps.{id}.repeat' must be a positive integer"));
        None
      }
    };
    if repeat.is_some() && do_while.is_some() {
      errors.push(format!("step '{id}' cannot have both 'repeat' and 'do-while'"));
    }
    let break_loop = match fm.get("break") {
      None => false,
      Some(Node::Scalar(s)) => match s.trim() {
        "true" | "yes" | "on" | "1" => true,
        "false" | "no" | "off" | "0" => false,
        other => {
          errors.push(format!("'steps.{id}.break': expected a boolean, got '{other}'"));
          false
        }
      },
      Some(_) => {
        errors.push(format!("'steps.{id}.break': expected a boolean"));
        false
      }
    };

    if let (Some(agent), Some(task)) = (agent, task) {
      steps.push(Step {
        id: id.to_string(),
        agent,
        task,
        inputs,
        outputs,
        when,
        needs,
        artifacts,
        commits,
        repeat,
        do_while,
        break_loop,
      });
    }
  }

  validate_step_graph(&steps, params, errors);
  steps
}

/// Resolve exactly one of a step's `prompt:` or `skill:` declarations. Bundled skill lookup is
/// deliberately strict: a typo fails definition discovery instead of becoming an agent prompt
/// that only fails after an expensive container starts.
fn validate_step_task(
  id: &str, prompt: Option<&Node>, skill: Option<&Node>, errors: &mut Vec<String>,
) -> Option<StepTask> {
  match (prompt, skill) {
    (Some(_), Some(_)) => {
      errors.push(format!("step '{id}' must declare exactly one of 'prompt' or 'skill', not both"));
      None
    }
    (None, None) => {
      errors.push(format!("step '{id}' is missing required key 'prompt' or 'skill'"));
      None
    }
    (Some(node), None) => required_scalar(Some(node), &format!("steps.{id}.prompt"), errors).map(StepTask::Prompt),
    (None, Some(Node::Scalar(name))) if !name.trim().is_empty() => {
      let name = name.trim();
      match config::bundled_skill_body(name) {
        Some(body) => Some(StepTask::BundledSkill { name: name.to_string(), body: body.to_string() }),
        None => {
          errors.push(format!("'steps.{id}.skill' names '{name}', which is not a bundled skill"));
          None
        }
      }
    }
    (None, Some(Node::Scalar(_))) => {
      errors.push(format!("'steps.{id}.skill' must not be empty"));
      None
    }
    (None, Some(Node::Map(_))) => {
      errors.push(format!("'steps.{id}.skill' must be a bundled skill name"));
      None
    }
  }
}

/// Validate a step's `agent:` block into a [`StepAgent`] (harness required; model/effort optional).
fn validate_step_agent(id: &str, node: Option<&Node>, errors: &mut Vec<String>) -> Option<StepAgent> {
  let fields = match node {
    None => {
      errors.push(format!("step '{id}' is missing required key 'agent'"));
      return None;
    }
    Some(Node::Map(f)) => f,
    Some(Node::Scalar(_)) => {
      errors.push(format!("'steps.{id}.agent' must be a mapping with 'harness' (and optional 'model'/'effort')"));
      return None;
    }
  };
  let mut fm: BTreeMap<&str, &Node> = BTreeMap::new();
  for (k, v) in fields {
    fm.insert(k.as_str(), v);
  }
  for (k, _) in fields {
    if !["harness", "model", "effort"].contains(&k.as_str()) {
      errors.push(format!("unknown key 'steps.{id}.agent.{k}' (allowed: harness, model, effort)"));
    }
  }
  let harness = match fm.get("harness").copied() {
    Some(Node::Scalar(s)) => match crate::config::Harness::parse(s.trim()) {
      Some(h) => Some(h),
      None => {
        errors.push(format!("'steps.{id}.agent.harness' is '{}', not a known harness", s.trim()));
        None
      }
    },
    _ => {
      errors.push(format!("'steps.{id}.agent' is missing 'harness'"));
      None
    }
  };
  let model = step_opt_scalar(&fm, id, "model", errors);
  let effort = step_opt_scalar(&fm, id, "effort", errors);
  harness.map(|harness| StepAgent { harness, model, effort })
}

/// Validate a step's `inputs:` block into bindings (env var name → source reference).
fn validate_step_inputs(id: &str, node: Option<&Node>, errors: &mut Vec<String>) -> Vec<InputBinding> {
  let entries = match node {
    None => return Vec::new(),
    Some(Node::Map(m)) => m,
    Some(Node::Scalar(_)) => {
      errors.push(format!("'steps.{id}.inputs' must be a mapping of NAME: source"));
      return Vec::new();
    }
  };
  let mut out = Vec::new();
  for (name, node) in entries {
    let name = name.trim();
    if !config::is_env_name(name) {
      errors.push(format!("'steps.{id}.inputs': '{name}' is not a valid variable name"));
      continue;
    }
    let src = match node {
      Node::Scalar(s) => s.trim(),
      Node::Map(_) => {
        errors.push(format!("'steps.{id}.inputs.{name}' must be a reference like params.X or stepid.field"));
        continue;
      }
    };
    match Ref::parse(src) {
      Some(reference) => out.push(InputBinding { name: name.to_string(), source: reference }),
      None => errors.push(format!("'steps.{id}.inputs.{name}' is '{src}', not a params.X or stepid.field reference")),
    }
  }
  out
}

/// Validate a step's `output:` block into typed fields the step must produce.
fn validate_step_outputs(id: &str, node: Option<&Node>, errors: &mut Vec<String>) -> Vec<OutputField> {
  let entries = match node {
    None => {
      errors.push(format!("step '{id}' is missing required key 'output' (the fields it must produce)"));
      return Vec::new();
    }
    Some(Node::Map(m)) if !m.is_empty() => m,
    _ => {
      errors.push(format!("'steps.{id}.output' must declare at least one field"));
      return Vec::new();
    }
  };
  let mut out = Vec::new();
  for (field, node) in entries {
    let field = field.trim();
    if !config::is_env_name(field) {
      errors.push(format!("'steps.{id}.output': '{field}' is not a valid field name"));
      continue;
    }
    let fm = match node {
      Node::Map(m) => m,
      Node::Scalar(_) => {
        errors.push(format!("'steps.{id}.output.{field}' must be a mapping with 'type'"));
        continue;
      }
    };
    let mut m: BTreeMap<&str, &Node> = BTreeMap::new();
    for (k, v) in fm {
      m.insert(k.as_str(), v);
    }
    for key in m.keys() {
      if !matches!(*key, "type" | "choices") {
        errors.push(format!("unknown key 'steps.{id}.output.{field}.{key}' (allowed: type, choices)"));
      }
    }
    let ty = match m.get("type").copied() {
      Some(Node::Scalar(s)) => match OutputType::parse(s.trim()) {
        Some(ty) => ty,
        None => {
          errors.push(format!("'steps.{id}.output.{field}.type' must be string, int, bool, enum, or string_list"));
          OutputType::String
        }
      },
      _ => {
        errors.push(format!("'steps.{id}.output.{field}' is missing required key 'type'"));
        OutputType::String
      }
    };
    let choices = match m.get("choices").copied() {
      Some(Node::Scalar(s)) => s.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
      _ => Vec::new(),
    };
    if ty == OutputType::Enum && choices.is_empty() {
      errors.push(format!("'steps.{id}.output.{field}' is an enum but has no 'choices'"));
    }
    if ty != OutputType::Enum && !choices.is_empty() {
      errors.push(format!("'steps.{id}.output.{field}.choices' is allowed only for enum outputs"));
    }
    out.push(OutputField { name: field.to_string(), ty, choices });
  }
  out
}

/// Validate a step's condition block map (`when:` or `do-while:`) into AND-ed conditions.
fn validate_step_cond_block(id: &str, key_name: &str, node: Option<&Node>, errors: &mut Vec<String>) -> Option<When> {
  let entries = match node {
    None => return None,
    Some(Node::Map(m)) if !m.is_empty() => m,
    _ => {
      errors.push(format!("'steps.{id}.{key_name}' must be a non-empty mapping of condition entries"));
      return None;
    }
  };
  let mut conds = Vec::new();
  for (key, node) in entries {
    let Some(reference) = Ref::parse(key.trim()) else {
      errors.push(format!("'steps.{id}.{key_name}': '{}' is not a params.X or stepid.field reference", key.trim()));
      continue;
    };
    let (op, values) = match node {
      // A scalar value → equality.
      Node::Scalar(s) => (CondOp::Eq, vec![s.trim().to_string()]),
      // A one-entry mapping → the named operator.
      Node::Map(m) if m.len() == 1 => {
        let (opk, opv) = &m[0];
        let Some(op) = CondOp::parse(opk.trim()) else {
          errors.push(format!("'steps.{id}.{key_name}.{}': unknown operator '{}'", key.trim(), opk.trim()));
          continue;
        };
        match opv {
          Node::Scalar(s) if op == CondOp::In => {
            (op, s.split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect())
          }
          Node::Scalar(s) => (op, vec![s.trim().to_string()]),
          Node::Map(_) => {
            errors.push(format!("'steps.{id}.{key_name}.{}.{}' must be a value", key.trim(), opk.trim()));
            continue;
          }
        }
      }
      Node::Map(_) => {
        errors.push(format!("'steps.{id}.{key_name}.{}' must be a value or a single operator mapping", key.trim()));
        continue;
      }
    };
    conds.push(Cond { reference, op, values });
  }
  (!conds.is_empty()).then_some(conds)
}

/// Parse a comma/space-separated scalar list (brackets optional): `needs: a, b` or `[a, b]`.
/// Shared by `needs:` and `artifacts:`.
fn parse_needs(node: Option<&Node>) -> Vec<String> {
  let Some(Node::Scalar(s)) = node else { return Vec::new() };
  s.trim()
    .trim_start_matches('[')
    .trim_end_matches(']')
    .split([',', ' '])
    .map(str::trim)
    .filter(|x| !x.is_empty())
    .map(str::to_string)
    .collect()
}

/// Cross-step checks: `needs` names defined steps; the graph is acyclic; every `inputs`/`when`
/// reference resolves to a declared param or an upstream step's declared output field, and any
/// referenced step is listed in `needs` (so the ordering that makes the value available is
/// explicit). One deliberate exception: a step inside a do-while body may take an INPUT from the
/// body's final step — a loop-carried reference, resolving to the PREVIOUS iteration's value
/// (empty on the first iteration) — with no `needs`, since that back-edge is what the loop is.
fn validate_step_graph(steps: &[Step], params: &[Param], errors: &mut Vec<String>) {
  use std::collections::BTreeSet;
  let ids: BTreeSet<&str> = steps.iter().map(|s| s.id.as_str()).collect();
  let param_names: BTreeSet<&str> = params.iter().map(|p| p.name.as_str()).collect();
  let output_of = |step: &str| steps.iter().find(|s| s.id == step).map(|s| &s.outputs);

  for s in steps {
    for need in &s.needs {
      if !ids.contains(need.as_str()) {
        errors.push(format!("step '{}' needs '{need}', which is not a defined step", s.id));
      }
      if need == &s.id {
        errors.push(format!("step '{}' cannot need itself", s.id));
      }
    }
    // A reference from inside a do-while body to the body's final step: legal as an input
    // (previous iteration's value), so the loop's data channel needs no committed files.
    let loop_carried = |reference: &Ref| -> bool {
      let Ref::StepField { step, .. } = reference else { return false };
      steps
        .iter()
        .any(|end| &end.id == step && end.do_while.is_some() && do_while_body(steps, end).contains(&s.id.as_str()))
    };
    let check_ref = |reference: &Ref, ctx: &str, needs_edge: bool, errors: &mut Vec<String>| match reference {
      Ref::Param(n) => {
        if !param_names.contains(n.as_str()) {
          errors.push(format!("step '{}' {ctx} references params.{n}, which is not a declared param", s.id));
        }
      }
      Ref::StepField { step, field } => {
        if needs_edge && !s.needs.iter().any(|n| n == step) {
          errors.push(format!("step '{}' {ctx} references {step}.{field} but does not 'needs: {step}'", s.id));
        }
        match output_of(step) {
          None => {
            errors.push(format!("step '{}' {ctx} references {step}.{field}, but '{step}' is not a defined step", s.id))
          }
          Some(outputs) if !outputs.iter().any(|o| &o.name == field) => errors.push(format!(
            "step '{}' {ctx} references {step}.{field}, which '{step}' does not declare in its output",
            s.id
          )),
          Some(_) => {}
        }
      }
    };
    for b in &s.inputs {
      check_ref(&b.source, &format!("input '{}'", b.name), !loop_carried(&b.source), errors);
    }
    for c in s.when.iter().flatten() {
      check_ref(&c.reference, "when", true, errors);
    }
    if let Some(start) = &s.do_while {
      if !ids.contains(start.as_str()) {
        errors.push(format!("step '{}' do-while starts at '{start}', which is not a defined step", s.id));
      } else if start != &s.id && !depends_transitively(steps, &s.id, start) {
        errors.push(format!("step '{}' do-while start '{start}' is not an ancestor of the final step", s.id));
      }
    }
    if s.break_loop {
      let loops: Vec<&Step> = steps
        .iter()
        .filter(|end| end.do_while.is_some() && do_while_body(steps, end).first().copied() == Some(s.id.as_str()))
        .collect();
      if loops.len() != 1 {
        errors.push(format!("step '{}' uses 'break: true' but is not the unique first step of a do-while body", s.id));
      }
      match s.outputs.iter().find(|o| o.name == "SCSH_LOOP_BREAK") {
        Some(field) if field.ty == OutputType::Bool => {}
        Some(_) => errors.push(format!("step '{}' output SCSH_LOOP_BREAK must have type bool", s.id)),
        None => {
          errors.push(format!("step '{}' uses 'break: true' and must declare boolean output SCSH_LOOP_BREAK", s.id))
        }
      }
    }
  }

  if let Some(cycle) = first_cycle(steps) {
    errors.push(format!("steps form a cycle via 'needs': {}", cycle.join(" → ")));
  }
}

fn depends_transitively(steps: &[Step], step: &str, ancestor: &str) -> bool {
  fn visit(steps: &[Step], step: &str, ancestor: &str, seen: &mut std::collections::BTreeSet<String>) -> bool {
    if !seen.insert(step.to_string()) {
      return false;
    }
    let Some(current) = steps.iter().find(|s| s.id == step) else { return false };
    current.needs.iter().any(|need| need == ancestor || visit(steps, need, ancestor, seen))
  }
  visit(steps, step, ancestor, &mut std::collections::BTreeSet::new())
}

/// The ordered ids in a do-while body, from its named first step through its final step.
pub fn do_while_body<'a>(steps: &'a [Step], end: &Step) -> Vec<&'a str> {
  let Some(start) = end.do_while.as_deref() else { return Vec::new() };
  steps
    .iter()
    .filter(|candidate| {
      (candidate.id == start || depends_transitively(steps, &candidate.id, start))
        && (candidate.id == end.id || depends_transitively(steps, &end.id, &candidate.id))
    })
    .map(|s| s.id.as_str())
    .collect()
}

/// Return a cycle in the `needs` graph (as a list of step ids) if one exists, via DFS.
fn first_cycle(steps: &[Step]) -> Option<Vec<String>> {
  use std::collections::BTreeMap as Map;
  let deps: Map<&str, &Vec<String>> = steps.iter().map(|s| (s.id.as_str(), &s.needs)).collect();
  // 0 = unvisited, 1 = on stack, 2 = done.
  let mut state: Map<&str, u8> = steps.iter().map(|s| (s.id.as_str(), 0u8)).collect();
  let mut stack: Vec<&str> = Vec::new();
  fn dfs<'a>(
    node: &'a str, deps: &Map<&'a str, &'a Vec<String>>, state: &mut Map<&'a str, u8>, stack: &mut Vec<&'a str>,
  ) -> Option<Vec<String>> {
    state.insert(node, 1);
    stack.push(node);
    if let Some(needs) = deps.get(node) {
      for n in needs.iter() {
        let n = n.as_str();
        match state.get(n).copied().unwrap_or(2) {
          1 => {
            let start = stack.iter().position(|x| *x == n).unwrap_or(0);
            let mut cyc: Vec<String> = stack[start..].iter().map(|s| s.to_string()).collect();
            cyc.push(n.to_string());
            return Some(cyc);
          }
          0 => {
            if let Some(c) = dfs(n, deps, state, stack) {
              return Some(c);
            }
          }
          _ => {}
        }
      }
    }
    stack.pop();
    state.insert(node, 2);
    None
  }
  for s in steps {
    if state.get(s.id.as_str()).copied().unwrap_or(2) == 0 {
      if let Some(c) = dfs(s.id.as_str(), &deps, &mut state, &mut stack) {
        return Some(c);
      }
    }
  }
  None
}

/// A step's optional scalar sub-field of `agent:`.
fn step_opt_scalar(fm: &BTreeMap<&str, &Node>, id: &str, field: &str, errors: &mut Vec<String>) -> Option<String> {
  match fm.get(field).copied() {
    None => None,
    Some(Node::Scalar(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
    Some(Node::Scalar(_)) => None,
    Some(Node::Map(_)) => {
      errors.push(format!("'steps.{id}.agent.{field}' must be a string"));
      None
    }
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
          errors.push(format!("'params.{name}.type' is '{}', not one of: string, text, int, bool, enum", s.trim()));
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

  #[test]
  fn builtin_arith_runs_three_steps_on_three_harnesses() {
    let def = builtin("arith");
    assert!(def.is_workflow());
    assert_eq!(def.steps.len(), 3);
    // Every param has a default, so the bundle runs on any opened directory with zero setup.
    assert_eq!(def.params.len(), 4);
    assert!(def.params.iter().all(|p| p.default.is_some()));
    // Three DIFFERENT harnesses — the whole point is watching a heterogeneous fleet.
    let harnesses: std::collections::BTreeSet<&str> = def.steps.iter().map(|s| s.agent.harness.as_str()).collect();
    assert_eq!(harnesses.len(), 3, "steps must ride three distinct harnesses");
    let summarize = def.steps.iter().find(|s| s.id == "summarize").expect("summarize step");
    assert_eq!(summarize.needs, vec!["add".to_string(), "multiply".to_string()]);
    assert_eq!(summarize.artifacts, vec!["summary.txt".to_string()]);
    // The rendered body carries the artifact contract, not just the JSON one.
    let body = summarize.render_skill_body();
    assert!(body.contains("Required files"), "got: {body}");
    assert!(body.contains("`summary.txt`"), "got: {body}");
  }

  #[test]
  fn builtin_greet_is_a_commit_enabled_fake_pr_workflow() {
    let def = builtin("greet");
    assert!(def.is_workflow());
    assert_eq!(def.steps.len(), 3);
    assert_eq!(def.steps[0].id, "scaffold");
    assert_eq!(def.steps[1].id, "implement");
    assert_eq!(def.steps[2].id, "describe");
    assert!(def.steps.iter().all(|s| s.commits), "every greet step brings commits back");
    assert_eq!(def.steps[1].needs, vec!["scaffold".to_string()]);
    assert_eq!(def.steps[2].needs, vec!["implement".to_string()]);
    let body = def.steps[0].render_skill_body();
    assert!(body.contains("## Commits"), "commit-enabled steps get the commits contract: {body}");
  }

  #[test]
  fn builtin_repeat_demo_declares_a_dynamic_three_iteration_commit_loop() {
    let def = builtin("demo-loop-repeat");
    assert_eq!(def.steps.len(), 2);
    assert_eq!(def.steps[0].id, "initialize");
    assert_eq!(def.steps[1].id, "increment");
    assert_eq!(def.steps[1].repeat, Some(3));
    assert_eq!(def.steps[1].needs, vec!["initialize".to_string()]);
    assert!(def.steps.iter().all(|s| s.commits));
    assert!(def.steps.iter().all(|s| s.agent.harness == crate::config::Harness::Codex));
    assert!(def.steps.iter().all(|s| s.agent.model.as_deref() == Some("gpt-5.6-luna")));
    assert!(def.steps.iter().all(|s| s.agent.effort.is_none()), "default effort: low skips commit instructions");
  }

  #[test]
  fn builtin_do_while_demo_declares_a_conditional_commit_loop() {
    let def = builtin("demo-loop-do-while");
    assert_eq!(def.steps.len(), 3);
    assert_eq!(def.steps[0].id, "initialize");
    let increment = &def.steps[1];
    assert_eq!(increment.id, "increment");
    assert_eq!(increment.needs, vec!["initialize".to_string()]);
    assert_eq!(increment.repeat, None);
    let compare = &def.steps[2];
    assert_eq!(compare.do_while.as_deref(), Some("increment"));
    assert_eq!(do_while_body(&def.steps, compare), ["increment", "compare"]);
    assert!(compare.render_skill_body().contains("SCSH_DO_WHILE_REPEAT"));
    assert!(def.steps.iter().all(|s| s.agent.harness == crate::config::Harness::Codex));
    assert!(def.steps.iter().all(|s| s.agent.model.as_deref() == Some("gpt-5.6-luna")));
    assert!(def.steps.iter().all(|s| s.agent.effort.is_none()), "default effort: low skips commit instructions");
  }

  #[test]
  fn builtin_break_demo_exits_from_the_first_do_while_step() {
    let def = builtin("demo-loop-break");
    assert_eq!(def.steps.len(), 4);
    let check = def.steps.iter().find(|s| s.id == "check").unwrap();
    let carry = def.steps.iter().find(|s| s.id == "carry").unwrap();
    assert!(check.break_loop);
    assert!(check.outputs.iter().any(|o| o.name == "SCSH_LOOP_BREAK" && o.ty == OutputType::Bool));
    assert_eq!(carry.do_while.as_deref(), Some("check"));
    assert_eq!(do_while_body(&def.steps, carry), ["check", "increment", "carry"]);
    assert!(check.render_skill_body().contains("exits this do-while immediately"));
  }

  #[test]
  fn builtin_gorgeous_pipeline_reviews_the_current_branch_in_a_loop() {
    let def = builtin("gorgeous-pipeline");
    // demo-beautiful-loop minus the scaffolded `implement` step: the branch already
    // carries the work, so the pipeline starts at `prepare`.
    assert_eq!(def.steps.len(), 34);
    assert!(def.steps.iter().all(|s| s.id != "implement"), "no scaffolding step");

    let prepare = def.steps.iter().find(|s| s.id == "prepare").unwrap();
    assert!(prepare.needs.is_empty(), "prepare is the first step");
    assert!(prepare.commits, "the PR description lands as a commit on the caller's branch");
    let prepare_words = prepare.task.body().split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(prepare_words.contains("Write or update `PR-DESCRIPTION.md`"), "updates an existing description too");

    let fix = def.steps.iter().find(|s| s.id == "fix").unwrap();
    assert_eq!(fix.needs, vec!["decide".to_string()]);
    assert!(fix.commits, "fixes come back as commits");

    // The loop: decide (breaks when the bar is met) … collect (do-while back to decide).
    let decide = def.steps.iter().find(|s| s.id == "decide").unwrap();
    assert!(decide.break_loop);
    assert!(decide.outputs.iter().any(|o| o.name == "SCSH_LOOP_BREAK" && o.ty == OutputType::Bool));
    let collect = def.steps.iter().find(|s| s.id == "collect").unwrap();
    assert_eq!(collect.do_while.as_deref(), Some("decide"));
    assert!(collect.outputs.iter().any(|o| o.name == "SCSH_DO_WHILE_REPEAT" && o.ty == OutputType::Bool));

    // The bundled route standard, both batches: 5 profiles × (Opus 4.8, Codex Terra, Cursor Auto).
    let initial: Vec<&Step> = def.steps.iter().filter(|s| s.id.starts_with("initial_")).collect();
    let reviewers: Vec<&Step> = def.steps.iter().filter(|s| s.id.starts_with("review_")).collect();
    assert_eq!((initial.len(), reviewers.len()), (15, 15));
    for batch in [&initial, &reviewers] {
      assert_eq!(batch.iter().filter(|r| r.id.ends_with("_opus")).count(), 5);
      assert_eq!(batch.iter().filter(|r| r.id.ends_with("_terra")).count(), 5);
      assert_eq!(batch.iter().filter(|r| r.id.ends_with("_cursor")).count(), 5);
    }
    for r in initial.iter().chain(&reviewers) {
      assert!(!r.commits, "reviewers are read-only");
      let words = r.task.body().split_whitespace().collect::<Vec<_>>().join(" ");
      assert!(
        words.contains("the commits since the current branch diverged from the repository's default branch"),
        "{} reviews THIS branch's change set, not the whole tree",
        r.id
      );
      assert!(
        words.contains("Never request, recommend, or create any additional PR-description section"),
        "{} enforces the PR-description policy",
        r.id
      );
      if r.id.ends_with("_terra") {
        assert_eq!(r.agent.harness, crate::config::Harness::Codex);
        assert_eq!(r.agent.effort.as_deref(), Some("high"));
      }
    }
  }

  #[test]
  fn builtin_beautiful_loop_demo_wires_a_review_panel_do_while() {
    let def = builtin("demo-beautiful-loop");
    assert_eq!(def.steps.len(), 35);

    let implement = def.steps.iter().find(|s| s.id == "implement").unwrap();
    let prepare = def.steps.iter().find(|s| s.id == "prepare").unwrap();
    let fix = def.steps.iter().find(|s| s.id == "fix").unwrap();
    assert!(implement.needs.is_empty() && implement.commits);
    assert_eq!(prepare.needs, vec!["implement".to_string()]);
    assert_eq!(fix.needs, vec!["decide".to_string()]);
    for coder in [implement, prepare, fix] {
      assert_eq!(coder.agent.harness, crate::config::Harness::Cursor);
      assert_eq!(coder.agent.model.as_deref(), Some("auto"));
      assert!(coder.commits);
    }
    for heading in ["## Summary", "## What This Changes", "## Implementation Details"] {
      assert!(prepare.task.body().contains(heading), "prepare pins {heading}");
      assert!(fix.task.body().contains(heading), "fix preserves {heading}");
    }
    assert!(fix.task.body().contains("required demo artifact"));

    let initial: Vec<&Step> = def.steps.iter().filter(|s| s.id.starts_with("initial_")).collect();
    let reviewers: Vec<&Step> = def.steps.iter().filter(|s| s.id.starts_with("review_")).collect();
    assert_eq!(initial.len(), 15);
    assert_eq!(reviewers.len(), 15);
    for profile in ["conventions", "justification", "reviewability", "sanity", "testing"] {
      assert_eq!(initial.iter().filter(|r| r.id.starts_with(&format!("initial_{profile}_"))).count(), 3);
      assert_eq!(reviewers.iter().filter(|r| r.id.starts_with(&format!("review_{profile}_"))).count(), 3);
    }
    for r in initial.iter().chain(&reviewers) {
      let expected_need = match r.id.as_str() {
        "initial_testing_cursor" => "initial_conventions_cursor",
        "initial_sanity_cursor" => "initial_justification_cursor",
        "review_testing_cursor" => "review_conventions_cursor",
        "review_sanity_cursor" => "review_justification_cursor",
        id if id.starts_with("initial_") => "prepare",
        _ => "fix",
      };
      assert_eq!(r.needs, vec![expected_need.to_string()]);
      assert!(!r.commits, "reviewers are read-only");
      if r.id.ends_with("_opus") {
        assert_eq!(r.agent.harness, crate::config::Harness::Claude);
        assert_eq!(r.agent.model.as_deref(), Some("claude-opus-4-8"));
        assert!(r.agent.effort.is_none());
      } else if r.id.ends_with("_terra") {
        assert_eq!(r.agent.harness, crate::config::Harness::Codex);
        assert_eq!(r.agent.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(r.agent.effort.as_deref(), Some("high"));
      } else {
        assert!(r.id.ends_with("_cursor"));
        assert_eq!(r.agent.harness, crate::config::Harness::Cursor);
        assert_eq!(r.agent.model.as_deref(), Some("auto"));
        assert!(r.agent.effort.is_none());
      }
      let grade = r.outputs.iter().find(|o| o.name == "grade").expect("every reviewer grades");
      assert_eq!(grade.ty, OutputType::Enum);
      assert_eq!(grade.choices, ["excellent", "good", "average", "poor"]);
      assert!(r.outputs.iter().any(|o| o.name == "comments" && o.ty == OutputType::StringList));
      assert!(!r.outputs.iter().any(|o| o.name == "comment_count"));
      let prompt_words = r.task.body().split_whitespace().collect::<Vec<_>>().join(" ");
      assert!(
        prompt_words.contains("`grade` as a string") && prompt_words.contains("`comments` as an array"),
        "{} must state the distinct grade and comments types without ambiguity",
        r.id
      );
      assert!(
        r.task.body().contains("Never request, recommend, or create any")
          && r.task.body().contains("additional PR-description section"),
        "{} must enforce the PR-description policy at the reviewer boundary",
        r.id
      );
      if r.id.contains("testing") {
        assert!(r.task.body().contains("PR-DESCRIPTION.md is") && r.task.body().contains("change narrative only"));
      }
      if r.id.contains("reviewability") {
        assert!(r.task.body().contains("required demo artifact"));
      }
    }
    for prefix in ["initial", "review"] {
      let direct_cursor_reviewers = reviewers
        .iter()
        .chain(&initial)
        .filter(|r| {
          r.id.starts_with(prefix)
            && r.id.ends_with("_cursor")
            && r.needs[0] == if prefix == "initial" { "prepare" } else { "fix" }
        })
        .count();
      assert_eq!(direct_cursor_reviewers, 3, "{prefix} review round must open exactly three Cursor lanes");
    }

    let decide = def.steps.iter().find(|s| s.id == "decide").unwrap();
    let collect = def.steps.iter().find(|s| s.id == "collect").unwrap();
    assert_eq!(decide.agent.harness, crate::config::Harness::Cursor);
    assert!(decide.break_loop);
    assert!(decide.outputs.iter().any(|o| o.name == "SCSH_LOOP_BREAK" && o.ty == OutputType::Bool));
    assert_eq!(collect.do_while.as_deref(), Some("decide"));
    assert!(collect.outputs.iter().any(|o| o.name == "approved" && o.ty == OutputType::Bool));
    assert!(collect.outputs.iter().any(|o| o.name == "feedback"));
    for boundary in [prepare, decide, fix, collect] {
      assert!(
        boundary.task.body().contains("additional") && boundary.task.body().contains("PR-description section"),
        "{} must preserve the PR-description invariant",
        boundary.id
      );
    }
    let body = do_while_body(&def.steps, collect);
    assert_eq!(body.len(), 18);
    assert_eq!(body.first(), Some(&"decide"));
    assert_eq!(body.last(), Some(&"collect"));
    assert!(body.contains(&"fix"));
    assert!(reviewers.iter().all(|r| body.contains(&r.id.as_str())));
    assert!(collect.render_skill_body().contains("SCSH_DO_WHILE_REPEAT"));
  }

  #[test]
  fn workflow_rejects_repeat_combined_with_do_while() {
    let src = wf("    needs: a\n    repeat: 2\n    do-while: a\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      y:\n        type: int");
    let err = validate("t", &src, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("cannot have both 'repeat' and 'do-while'")), "{err:?}");
  }

  #[test]
  fn workflow_break_must_be_the_first_step_and_declare_a_boolean_result() {
    let outside = wf(
      "    break: true\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      SCSH_LOOP_BREAK:\n        type: bool",
    );
    let err = validate("t", &outside, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("not the unique first step")), "{err:?}");

    let missing = wf(
      "    break: true\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n  b:\n    needs: a\n    do-while: a\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      SCSH_DO_WHILE_REPEAT:\n        type: bool",
    );
    let err = validate("t", &missing, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("must declare boolean output SCSH_LOOP_BREAK")), "{err:?}");

    let wrong_type = missing.replace(
      "    prompt: |\n      go\n  b:",
      "    prompt: |\n      go\n    output:\n      SCSH_LOOP_BREAK:\n        type: string\n  b:",
    );
    let err = validate("t", &wrong_type, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("SCSH_LOOP_BREAK must have type bool")), "{err:?}");
  }

  #[test]
  fn do_while_names_an_ancestor_and_rejects_comparator_blocks() {
    let ok = wf("    needs: a\n    do-while: a\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      y:\n        type: int");
    let def = validate("t", &ok, DefSource::Repo).unwrap_or_else(|e| panic!("{}", e.join("; ")));
    assert_eq!(def.steps.iter().find(|s| s.id == "b").unwrap().do_while.as_deref(), Some("a"));

    let bad = wf("    needs: a\n    do-while:\n      b.y:\n        lt: 3\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      y:\n        type: int");
    let err = validate("t", &bad, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("not a comparator block")), "{err:?}");
  }

  #[test]
  fn do_while_start_must_be_an_ancestor() {
    let src = wf("    do-while: a\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      y:\n        type: int");
    let err = validate("t", &src, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("not an ancestor")), "{err:?}");
  }

  /// A do-while body where the START step consumes the END step's output — the loop-carried
  /// channel (previous iteration's value; empty on round one) — plus a step outside the body.
  fn loop_carried_wf(a_input: &str, c_input: &str) -> String {
    format!(
      "description: \"x\"\nsteps:\n  a:\n    inputs:\n      PREV: {a_input}\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      kind:\n        type: string\n  b:\n    needs: a\n    do-while: a\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      y:\n        type: int\n  c:\n    inputs:\n      PREV: {c_input}\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      go\n    output:\n      z:\n        type: string\n"
    )
  }

  #[test]
  fn loop_carried_input_from_the_do_while_end_needs_no_edge() {
    // `a` (the body's first step) reads `b.y` (the body's final step) with no `needs: b` —
    // that back-edge IS the loop, so it validates; `c` reads a param to stay neutral.
    let src = loop_carried_wf("b.y", "params.P")
      .replace("steps:", "params:\n  P:\n    type: string\n    default: \"\"\nsteps:");
    let def = validate("t", &src, DefSource::Repo).unwrap_or_else(|e| panic!("{}", e.join("; ")));
    let a = def.steps.iter().find(|s| s.id == "a").unwrap();
    assert_eq!(a.inputs[0].source, Ref::StepField { step: "b".into(), field: "y".into() });
    assert!(a.needs.is_empty(), "the loop-carried reference adds no needs edge (that would be a cycle)");
  }

  #[test]
  fn loop_carried_input_still_requires_a_declared_output_field() {
    let src = loop_carried_wf("b.missing", "b.y");
    let err = validate("t", &src, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("b.missing") && e.contains("does not declare")), "{err:?}");
    // And a step OUTSIDE the do-while body gets no back-edge exemption: `c` must needs: b.
    assert!(err.iter().any(|e| e.contains("step 'c'") && e.contains("does not 'needs: b'")), "{err:?}");
  }

  #[test]
  fn builtin_demo_pr_is_a_commit_enabled_multi_agent_flat_def() {
    let def = builtin("demo-pr");
    assert!(!def.is_workflow(), "demo-pr is a flat one-shot, not a DAG");
    assert_eq!(def.invocations.len(), 4);
    let agents: std::collections::BTreeSet<&str> = def.invocations.iter().map(|r| r.harness.as_str()).collect();
    assert_eq!(agents, ["claude", "codex", "cursor", "grok"].into_iter().collect());
    assert!(
      def.invocations.iter().all(|r| r.commits == Some(true)),
      "every demo-pr route is commit-enabled for packdiff"
    );
    let task = def.task.as_deref().expect("flat def has a task");
    assert!(task.contains("PR-DESCRIPTION.md"), "task writes the notes file: {task}");
    assert!(task.contains("demo_pr_note.txt"), "task writes a feature stub: {task}");
    let title = def.params.iter().find(|p| p.name == "TITLE").expect("TITLE param");
    assert_eq!(title.default.as_deref(), Some("Hello from demo-pr"));
  }

  #[test]
  fn builtin_smoke_pr_defs_are_one_harness_each() {
    let expected = [
      ("smoke-pr-claude", "claude", "sonnet"),
      ("smoke-pr-codex", "codex", "gpt-5.6-luna"),
      ("smoke-pr-grok", "grok", "grok-composer-2.5-fast"),
      ("smoke-pr-cursor", "cursor", "composer-2.5-fast"),
    ];
    for (name, harness, model) in expected {
      let def = builtin(name);
      assert!(!def.is_workflow(), "{name} is a flat one-shot");
      assert_eq!(def.invocations.len(), 1, "{name} is a single-harness smoke");
      let route = &def.invocations[0];
      assert_eq!(route.harness.as_str(), harness, "{name}");
      assert_eq!(route.model.as_deref(), Some(model), "{name}");
      assert_eq!(route.commits, Some(true), "{name} commits for packdiff");
      let task = def.task.as_deref().expect("task");
      assert!(task.contains("PR-DESCRIPTION.md") && task.contains("demo_pr_note.txt"), "{name}: {task}");
      let skill = def.to_skill();
      let cfg = crate::config::Config { skills: vec![skill], terminal: crate::config::Terminal::default() };
      let inv = crate::config::expand_invocations(&cfg);
      assert_eq!(inv.len(), 1);
      assert_eq!(inv[0].name, format!("{name}-run"));
      assert_eq!(inv[0].harness.as_str(), harness);
    }
  }

  #[test]
  fn step_commits_parses_as_a_boolean() {
    let ok = r#"description: "x"
steps:
  s1:
    agent:
      harness: claude
    prompt: "p"
    output:
      ok:
        type: bool
    commits: true
"#;
    let def = validate("t", ok, DefSource::Builtin).unwrap();
    assert!(def.steps[0].commits);
    let bad = r#"description: "x"
steps:
  s1:
    agent:
      harness: claude
    prompt: "p"
    output:
      ok:
        type: bool
    commits: maybe
"#;
    let err = validate("t", bad, DefSource::Builtin).unwrap_err();
    assert!(err.iter().any(|e| e.contains("commits")), "{err:?}");
  }

  #[test]
  fn step_artifacts_must_be_plain_filenames() {
    let bad = r#"description: "x"
steps:
  s1:
    agent:
      harness: claude
    prompt: p
    artifacts: ../escape.txt
    output:
      ok:
        type: string
"#;
    let err = validate("t", bad, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("must be a plain filename")), "got: {err:?}");
  }

  #[test]
  fn builtin_code_review_probes_credentials_then_reviews() {
    let def = builtin("code-review");
    assert!(def.is_workflow());
    assert_eq!(def.steps.len(), 2);
    let probe = &def.steps[0];
    assert_eq!(probe.id, "probe_credentials");
    assert!(probe.needs.is_empty() && probe.when.is_none());
    assert_eq!(probe.outputs.len(), 1);
    let review = &def.steps[1];
    assert_eq!(review.id, "review");
    // The review runs only after — and only if — the probe succeeded end to end.
    assert_eq!(review.needs, vec!["probe_credentials".to_string()]);
    let when = review.when.as_ref().expect("review is gated on the probe");
    assert_eq!(when.len(), 1);
    assert_eq!(when[0].reference, Ref::StepField { step: "probe_credentials".into(), field: "ok".into() });
    assert_eq!(when[0].op, CondOp::Eq);
    assert_eq!(when[0].values, vec!["true".to_string()]);
    assert_eq!(format_when_summary(when), "Runs only if probe_credentials.ok = true");
  }

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
    let task = add.task.as_deref().expect("flat def has a task");
    assert!(task.contains('\n'), "task should be multi-line");
    assert!(task.contains("SCSH_RESULT"), "task body preserved");
    // add spins up every non-opencode agent: codex, claude, cursor, grok.
    assert_eq!(add.invocations.len(), 4);
    let agents: std::collections::BTreeSet<&str> = add.invocations.iter().map(|r| r.harness.as_str()).collect();
    assert_eq!(agents, ["claude", "codex", "cursor", "grok"].into_iter().collect());
    assert!(!agents.contains("opencode"), "opencode is intentionally excluded");

    let research = builtin("research");
    let city = research.params.iter().find(|p| p.name == "CITY").unwrap();
    assert!(city.required && city.default.is_none(), "CITY is required with no default");
    let area = research.params.iter().find(|p| p.name == "AREA").unwrap();
    assert!(!area.required && area.default.as_deref() == Some(""), "AREA optional, empty default");

    let doctor = builtin("doctor");
    assert!(doctor.params.is_empty());
    // doctor exercises every agent end to end — all five harnesses.
    assert_eq!(doctor.invocations.len(), 5);
    let doc_agents: std::collections::BTreeSet<&str> = doctor.invocations.iter().map(|r| r.harness.as_str()).collect();
    assert_eq!(doc_agents, ["claude", "codex", "cursor", "grok", "opencode"].into_iter().collect());

    let build = builtin("big-beautiful-build");
    assert!(build.is_workflow());
    assert_eq!(build.params.len(), 1);
    assert_eq!(build.params[0].name, "FEATURE");
    assert_eq!(build.params[0].ty, ParamType::Text);
    assert!(build.params[0].required);
    let step = &build.steps[0];
    assert_eq!(step.agent.harness, crate::config::Harness::Cursor);
    assert_eq!(step.agent.model.as_deref(), Some("auto"));
    assert!(step.commits);
    assert_eq!(step.artifacts, ["big-beautiful-build.md"]);
    match &step.task {
      StepTask::BundledSkill { name, body } => {
        assert_eq!(name, "big-beautiful-build");
        assert_eq!(body, crate::config::bundled_skill_body(name).expect("canonical bundled body"));
      }
      StepTask::Prompt(_) => panic!("the built-in must execute the canonical skill, not a copied prompt"),
    }
    assert!(step.render_skill_body().contains("FEATURE"));
    assert!(step.render_skill_body().contains("big-beautiful-build.md"));
  }

  #[test]
  fn workflow_step_requires_exactly_one_valid_task_source() {
    let both = "description: x\nsteps:\n  s:\n    agent:\n      harness: cursor\n    prompt: go\n    skill: big-beautiful-build\n    output:\n      ok:\n        type: bool\n";
    let err = validate("t", both, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("exactly one of 'prompt' or 'skill'")), "{err:?}");

    let unknown = "description: x\nsteps:\n  s:\n    agent:\n      harness: cursor\n    skill: typo\n    output:\n      ok:\n        type: bool\n";
    let err = validate("t", unknown, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("not a bundled skill")), "{err:?}");
  }

  #[test]
  fn builtins_never_generate_pr_checklists() {
    let forbidden = ["test", "plan"].join(" ");
    for (name, src) in builtin_defs() {
      assert!(
        !src.to_ascii_lowercase().contains(&forbidden),
        "built-in definition {name} recommends a forbidden PR section"
      );
    }
  }

  #[test]
  fn builtin_fruits_workflow_parses() {
    let f = builtin("fruits");
    assert!(f.is_workflow(), "fruits is a workflow");
    assert!(f.task.is_none() && f.invocations.is_empty(), "a workflow has no flat task/invocations");
    assert_eq!(f.steps.len(), 3);
    let categorize = f.steps.iter().find(|s| s.id == "categorize").unwrap();
    assert!(categorize.needs.is_empty() && categorize.outputs.iter().any(|o| o.name == "fruits"));
    let sort_fruits = f.steps.iter().find(|s| s.id == "sort_fruits").unwrap();
    assert_eq!(sort_fruits.needs, vec!["categorize"]);
    // Its LIST input binds to categorize.fruits.
    let bind = sort_fruits.inputs.iter().find(|b| b.name == "LIST").unwrap();
    assert_eq!(bind.source, Ref::StepField { step: "categorize".into(), field: "fruits".into() });
  }

  /// A minimal two-step workflow source for negative tests.
  fn wf(extra_second: &str) -> String {
    format!(
      "description: \"x\"\nsteps:\n  a:\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      do a\n    output:\n      kind:\n        type: string\n  b:\n{extra_second}\n"
    )
  }

  #[test]
  fn workflow_rejects_reference_to_undeclared_output() {
    // b references a.missing, which a does not declare in its output.
    let src = wf("    needs: a\n    agent:\n      harness: claude\n      model: sonnet\n    inputs:\n      X: a.missing\n    prompt: |\n      go\n    output:\n      y:\n        type: string");
    let err = validate("t", &src, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("a.missing")), "{err:?}");
  }

  #[test]
  fn workflow_rejects_reference_without_needs() {
    // b references a.kind but does not declare needs: a.
    let src = wf("    agent:\n      harness: claude\n      model: sonnet\n    inputs:\n      X: a.kind\n    prompt: |\n      go\n    output:\n      y:\n        type: string");
    let err = validate("t", &src, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("does not 'needs: a'")), "{err:?}");
  }

  #[test]
  fn workflow_rejects_cycles() {
    let src = "description: \"x\"\nsteps:\n  a:\n    needs: b\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      a\n    output:\n      y:\n        type: string\n  b:\n    needs: a\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      b\n    output:\n      y:\n        type: string\n";
    let err = validate("t", src, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("cycle")), "{err:?}");
  }

  #[test]
  fn workflow_and_flat_are_mutually_exclusive() {
    let src = "description: \"x\"\ntask: |\n  do\ninvocations:\n  c:\n    harness: claude\n    model: sonnet\nsteps:\n  a:\n    agent:\n      harness: claude\n      model: sonnet\n    prompt: |\n      a\n    output:\n      y:\n        type: string\n";
    let err = validate("t", src, DefSource::Repo).unwrap_err();
    assert!(err.iter().any(|e| e.contains("either 'steps:'")), "{err:?}");
  }

  #[test]
  fn condition_evaluation() {
    let refv = Ref::StepField { step: "s".into(), field: "n".into() };
    let ge = Cond { reference: refv.clone(), op: CondOp::Gte, values: vec!["3".into()] };
    assert!(ge.eval(&|_| Some("5".into())));
    assert!(!ge.eval(&|_| Some("2".into())));
    let eq = Cond { reference: refv.clone(), op: CondOp::Eq, values: vec!["code".into()] };
    assert!(eq.eval(&|_| Some("code".into())));
    assert!(!eq.eval(&|_| Some("docs".into())));
    // A missing value never satisfies a condition.
    assert!(!eq.eval(&|_| None));
  }

  #[test]
  fn step_body_carries_the_io_contract() {
    let f = builtin("fruits");
    let body = f.steps.iter().find(|s| s.id == "categorize").unwrap().render_skill_body();
    assert!(body.contains("WORDS"), "names the input");
    assert!(body.contains("$SCSH_RESULT"), "points at the result file");
    assert!(body.contains("fruits") && body.contains("vegetables"), "lists the output fields");
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

    let text = Param {
      name: "FEATURE".into(),
      ty: ParamType::Text,
      default: None,
      required: true,
      description: None,
      choices: vec![],
    };
    assert!(text.validate_value("first line\nsecond line").is_ok());
    assert!(text.validate_value(" \n ").is_err());
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
    assert_eq!(inv.len(), 4);
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
