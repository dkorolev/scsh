//! Setup tab payload: per-harness image + login readiness and the canonical model catalog.
//!
//! `GET /api/v1/setup` composes existing image inspect + host login preflight into a
//! browser-safe JSON shape. No secrets, no automatic model calls — probes are started
//! explicitly via `POST /api/v1/setup/tests`.

use crate::config::Harness;
use crate::json::quote;
use crate::runtime::{self, ImageStatus, LoginPreflight};

/// One curated catalog entry (primary smoke or additional built-in).
pub struct CatalogModel {
  pub id: &'static str,
  /// `"primary"` (default selected smoke) or `"builtin"` (shown, opt-in).
  pub kind: &'static str,
}

/// Canonical harness → models matrix shared with the built-in `doctor` definition.
/// Primary entries match `doctor.yml`; additional entries cover demo/builtin aliases.
pub fn catalog_models(harness: Harness) -> &'static [CatalogModel] {
  match harness {
    Harness::Opencode => &[
      CatalogModel { id: "openai/gpt-5.6-luna", kind: "primary" },
      CatalogModel { id: "openai/gpt-5.6-terra", kind: "builtin" },
    ],
    Harness::Claude => {
      &[CatalogModel { id: "sonnet", kind: "primary" }, CatalogModel { id: "claude-opus-4-8", kind: "builtin" }]
    }
    Harness::Codex => {
      &[CatalogModel { id: "gpt-5.6-luna", kind: "primary" }, CatalogModel { id: "gpt-5.6-terra", kind: "builtin" }]
    }
    Harness::Grok => {
      &[CatalogModel { id: "grok-build", kind: "primary" }, CatalogModel { id: "grok-4.5", kind: "builtin" }]
    }
    Harness::Cursor => {
      &[CatalogModel { id: "auto", kind: "primary" }, CatalogModel { id: "composer-2.5", kind: "builtin" }]
    }
  }
}

/// Primary smoke model id for a harness (first `primary` catalog entry).
pub fn primary_model_id(harness: Harness) -> &'static str {
  catalog_models(harness)
    .iter()
    .find(|m| m.kind == "primary")
    .map(|m| m.id)
    .expect("every harness has a primary catalog model")
}

/// Validate a custom model id from the browser (no secrets, no shell metacharacters).
pub fn validate_custom_model_id(raw: &str) -> Result<String, String> {
  let id = raw.trim();
  if id.is_empty() {
    return Err("model id is empty".into());
  }
  if id.len() > 128 {
    return Err("model id is too long (max 128 characters)".into());
  }
  if id.chars().any(|c| c.is_control() || c == '\n' || c == '\r' || c == '"' || c == '\'' || c == '`' || c == '$') {
    return Err("model id has unsafe characters".into());
  }
  Ok(id.to_string())
}

fn image_status_word(img: &ImageStatus) -> &'static str {
  if !img.exists {
    "missing"
  } else if !img.up_to_date {
    "stale"
  } else {
    "ready"
  }
}

fn image_json(s: &ImageStatus) -> String {
  format!(
    "{{ \"name\": {}, \"tag\": {}, \"exists\": {}, \"up_to_date\": {}, \"status\": {}, \"created\": {}, \"size\": {} }}",
    quote(&s.name),
    quote(&s.tag),
    s.exists,
    s.up_to_date,
    quote(image_status_word(s)),
    s.created.as_deref().map(quote).unwrap_or_else(|| "null".into()),
    s.size.as_deref().map(quote).unwrap_or_else(|| "null".into()),
  )
}

fn login_json(login: &LoginPreflight) -> String {
  format!(
    "{{ \"status\": {}, \"label\": {}, \"hint\": {} }}",
    quote(login.status),
    quote(&login.label),
    quote(&login.hint),
  )
}

fn models_json(harness: Harness) -> String {
  let primary = primary_model_id(harness);
  let parts: Vec<String> = catalog_models(harness)
    .iter()
    .map(|m| {
      format!(
        "{{ \"id\": {}, \"kind\": {}, \"primary_smoke\": {}, \"status\": \"not_tested\" }}",
        quote(m.id),
        quote(m.kind),
        m.id == primary,
      )
    })
    .collect();
  format!("[{}]", parts.join(", "))
}

/// Overall harness readiness for Setup (no end-to-end model probes yet on GET).
/// Never claims full "ready" — that requires passed model tests.
fn overall_status(image: &ImageStatus, login: &LoginPreflight) -> &'static str {
  if !image.exists || !image.up_to_date {
    "needs_build"
  } else if login.status != "found" {
    "needs_login"
  } else {
    "not_tested"
  }
}

fn overall_label(overall: &str) -> &'static str {
  match overall {
    "needs_build" => "Needs build",
    "needs_login" => "Needs login",
    "not_tested" => "Ready to test",
    _ => "Unknown",
  }
}

fn action_json(image: &ImageStatus, login: &LoginPreflight, overall: &str) -> String {
  match overall {
    "needs_build" => {
      let (kind, label) = if !image.exists { ("build", "Build image") } else { ("update", "Update image") };
      format!(
        "{{ \"kind\": {}, \"label\": {}, \"hint\": {} }}",
        quote(kind),
        quote(label),
        quote("Build this harness image for the selected runtime"),
      )
    }
    "needs_login" => {
      format!("{{ \"kind\": \"login\", \"label\": {}, \"hint\": {} }}", quote("Sign in on host"), quote(&login.hint),)
    }
    _ => {
      r#"{ "kind": "test", "label": "Test selected models", "hint": "Runs a real container probe for each checked model (provider calls may incur cost)" }"#
        .to_string()
    }
  }
}

/// `GET /api/v1/setup` — readiness dashboard for the Setup tab.
pub fn setup_json(runtime_override: Option<&str>) -> String {
  let available = runtime::available_runtimes();
  let rt_name: String = match runtime_override.filter(|r| !r.is_empty()) {
    Some(rt) if available.contains(&rt) => rt.to_string(),
    Some(rt) => {
      return format!(
        "{{ \"error\": {} }}",
        quote(&format!("runtime '{rt}' is not installed (available: {})", available.join(", ")))
      );
    }
    None => match runtime::detect_runtime() {
      Some(rt) => rt.name,
      None => {
        return r#"{ "error": "no container runtime found (docker, podman, or Apple container)" }"#.to_string();
      }
    },
  };
  let available_json: Vec<String> = available.iter().map(|r| quote(r)).collect();
  let statuses = runtime::image_statuses(&rt_name);
  let images_json: Vec<String> = statuses.iter().map(image_json).collect();
  let base = statuses.iter().find(|s| s.name == "base");

  let mut needs_build = 0u32;
  let mut needs_login = 0u32;
  let mut not_tested = 0u32;
  let mut harness_rows = Vec::new();

  for h in Harness::ALL {
    let img = statuses.iter().find(|s| s.name == h.as_str()).cloned().unwrap_or(ImageStatus {
      name: h.as_str().into(),
      tag: runtime::image_tag(h),
      exists: false,
      up_to_date: false,
      created: None,
      size: None,
    });
    let login = runtime::harness_login_preflight(h);
    let overall = overall_status(&img, &login);
    match overall {
      "needs_build" => needs_build += 1,
      "needs_login" => needs_login += 1,
      _ => not_tested += 1,
    }
    harness_rows.push(format!(
      "{{ \"id\": {}, \"name\": {}, \"overall\": {}, \"overall_label\": {}, \"image\": {}, \"login\": {}, \"models\": {}, \"action\": {} }}",
      quote(h.as_str()),
      quote(h.display_name()),
      quote(overall),
      quote(overall_label(overall)),
      image_json(&img),
      login_json(&login),
      models_json(h),
      action_json(&img, &login, overall),
    ));
  }

  let checked_at = crate::daemon::paths::now_unix_secs();
  format!(
    "{{ \"runtime\": {}, \"available\": [{}], \"checked_at\": {}, \"summary\": {{ \"needs_build\": {}, \"needs_login\": {}, \"not_tested\": {}, \"agents\": {} }}, \"harnesses\": [{}], \"base\": {}, \"images\": [{}] }}",
    quote(&rt_name),
    available_json.join(", "),
    checked_at,
    needs_build,
    needs_login,
    not_tested,
    Harness::ALL.len(),
    harness_rows.join(", "),
    base.map(image_json).unwrap_or_else(|| "null".into()),
    images_json.join(", "),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn setup_json_error_when_runtime_unknown() {
    let j = setup_json(Some("not-a-runtime-xyz"));
    assert!(j.contains("\"error\""), "{j}");
    assert!(j.contains("not-a-runtime-xyz"), "{j}");
  }

  #[test]
  fn catalog_marks_one_primary_per_harness() {
    for h in Harness::ALL {
      let cats = catalog_models(h);
      assert!(!cats.is_empty(), "{h:?}");
      assert_eq!(cats.iter().filter(|m| m.kind == "primary").count(), 1, "{h:?}");
    }
  }

  #[test]
  fn setup_orders_native_harnesses_before_opencode() {
    assert_eq!(Harness::ALL, [Harness::Claude, Harness::Codex, Harness::Grok, Harness::Cursor, Harness::Opencode]);
  }

  #[test]
  fn doctor_primaries_are_in_the_catalog() {
    let expected = [
      (Harness::Opencode, "openai/gpt-5.6-luna"),
      (Harness::Claude, "sonnet"),
      (Harness::Codex, "gpt-5.6-luna"),
      (Harness::Grok, "grok-build"),
      (Harness::Cursor, "auto"),
    ];
    for (h, id) in expected {
      assert_eq!(primary_model_id(h), id, "{h:?}");
    }
    assert!(!Harness::ALL.into_iter().flat_map(catalog_models).any(|model| model.id.contains("mini")));
  }

  #[test]
  fn custom_model_validation_rejects_unsafe() {
    assert!(validate_custom_model_id("").is_err());
    assert!(validate_custom_model_id("  ").is_err());
    assert!(validate_custom_model_id(&"a".repeat(200)).is_err());
    assert!(validate_custom_model_id("bad`id").is_err());
    assert_eq!(validate_custom_model_id("ok/model-1.2").unwrap(), "ok/model-1.2");
  }

  #[test]
  fn overall_label_for_untested_is_actionable() {
    assert_eq!(super::overall_label("not_tested"), "Ready to test");
  }

  #[test]
  fn login_preflight_disabled_when_opted_out() {
    std::env::set_var("SCSH_NO_CLAUDE_AUTH", "1");
    let login = runtime::harness_login_preflight(Harness::Claude);
    std::env::remove_var("SCSH_NO_CLAUDE_AUTH");
    assert_eq!(login.status, "disabled");
    assert!(login.label.contains("Disabled"));
    assert!(login.hint.contains("SCSH_NO_CLAUDE_AUTH"));
  }

  #[test]
  fn parse_setup_tests_accepts_a_batch() {
    use crate::json::{parse, Value};
    let Value::Object(obj) = parse(
      r#"{"tests":[{"harness":"claude","model":"sonnet"},{"harness":"codex","model":"gpt-5.6-luna","effort":"low"}]}"#,
    )
    .unwrap() else {
      panic!("object");
    };
    let tests = parse_setup_tests(&obj).unwrap();
    assert_eq!(tests.len(), 2);
    assert_eq!(tests[0].harness, Harness::Claude);
    assert_eq!(tests[0].model, "sonnet");
    assert_eq!(tests[1].effort.as_deref(), Some("low"));
  }

  #[test]
  fn render_setup_batch_yaml_lists_invocations() {
    let yaml = render_setup_batch_yaml(&[SetupTestRequest {
      harness: Harness::Grok,
      model: "grok-composer-2.5-fast".into(),
      effort: None,
    }]);
    assert!(yaml.contains("harness: grok"), "{yaml}");
    assert!(yaml.contains("model: grok-composer-2.5-fast"), "{yaml}");
  }
}

/// One requested model probe from `POST /api/v1/setup/tests`.
#[derive(Clone, Debug)]
pub struct SetupTestRequest {
  pub harness: Harness,
  pub model: String,
  pub effort: Option<String>,
}

const SETUP_TESTS_PROJECT: &str = "setup-tests";
// Runs and their result files are named `smoketest-<harness>-<model>` after this def.
const SETUP_BATCH_DEF: &str = "smoketest";
const MAX_SETUP_TESTS: usize = 10;

/// Parse and validate the `tests` array from a setup-tests POST body.
pub fn parse_setup_tests(obj: &[(String, crate::json::Value)]) -> Result<Vec<SetupTestRequest>, String> {
  use crate::json::Value;
  let Some((_, Value::Array(arr))) = obj.iter().find(|(k, _)| k == "tests") else {
    return Err("give a non-empty tests array".into());
  };
  if arr.is_empty() {
    return Err("give a non-empty tests array".into());
  }
  if arr.len() > MAX_SETUP_TESTS {
    return Err(format!("at most {MAX_SETUP_TESTS} model tests per request"));
  }
  let mut out = Vec::new();
  let mut seen = std::collections::BTreeSet::new();
  for item in arr {
    let Value::Object(fields) = item else {
      return Err("each test must be an object".into());
    };
    let harness_s = fields
      .iter()
      .find(|(k, _)| k == "harness")
      .and_then(|(_, v)| match v {
        Value::String(s) => Some(s.as_str()),
        _ => None,
      })
      .ok_or_else(|| "each test needs a harness".to_string())?;
    let harness = Harness::parse(harness_s).ok_or_else(|| format!("unknown harness '{harness_s}'"))?;
    let model_raw = fields
      .iter()
      .find(|(k, _)| k == "model")
      .and_then(|(_, v)| match v {
        Value::String(s) => Some(s.as_str()),
        _ => None,
      })
      .ok_or_else(|| "each test needs a model".to_string())?;
    let model = validate_custom_model_id(model_raw)?;
    let effort = fields.iter().find(|(k, _)| k == "effort").and_then(|(_, v)| match v {
      Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
      _ => None,
    });
    if let Some(ref e) = effort {
      let allowed = harness.effort_levels();
      if !allowed.is_empty() && !allowed.contains(&e.as_str()) {
        return Err(format!("effort '{e}' is not valid for {}", harness.as_str()));
      }
    }
    let key = format!("{}::{}", harness.as_str(), model);
    if !seen.insert(key) {
      return Err(format!("duplicate test for {} / {model}", harness.as_str()));
    }
    out.push(SetupTestRequest { harness, model, effort });
  }
  Ok(out)
}

/// Ensure `~/.scsh/projects/setup-tests` exists and is runnable; write the batch def YAML.
pub fn prepare_setup_batch(tests: &[SetupTestRequest]) -> Result<std::path::PathBuf, String> {
  let root = ensure_setup_tests_project()?;
  let harness_dir = root.join(".harness");
  std::fs::create_dir_all(&harness_dir).map_err(|e| format!("could not create .harness/: {e}"))?;
  let yaml = render_setup_batch_yaml(tests);
  let path = harness_dir.join(format!("{SETUP_BATCH_DEF}.yml"));
  std::fs::write(&path, yaml).map_err(|e| format!("could not write smoketest def: {e}"))?;
  Ok(root)
}

fn ensure_setup_tests_project() -> Result<std::path::PathBuf, String> {
  let projects = crate::daemon::paths::projects_dir();
  std::fs::create_dir_all(&projects).map_err(|e| format!("could not create {}: {e}", projects.display()))?;
  let path = projects.join(SETUP_TESTS_PROJECT);
  if path.join(".git").exists() {
    // Keep the repo clean for the next run: drop untracked leftovers except .harness/smoketest.yml
    // which we overwrite. Soft reset is unnecessary — blockers only care about committed+clean.
    let _ = std::process::Command::new("git").args(["-C"]).arg(&path).args(["checkout", "--", "."]).status();
    let _ = std::process::Command::new("git").args(["-C"]).arg(&path).args(["clean", "-fd", "-e", ".harness"]).status();
    return Ok(path);
  }
  // Reuse the same scaffold as Projects → New project.
  if path.exists() {
    let _ = std::fs::remove_dir_all(&path);
  }
  scaffold_setup_tests_project(&path)?;
  Ok(path)
}

fn scaffold_setup_tests_project(path: &std::path::Path) -> Result<(), String> {
  let git = |args: &[&str]| -> Result<(), String> {
    let out = std::process::Command::new("git")
      .arg("-C")
      .arg(path)
      .args(args)
      .output()
      .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
    if out.status.success() {
      Ok(())
    } else {
      Err(format!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()))
    }
  };
  std::fs::create_dir(path).map_err(|e| format!("could not create {}: {e}", path.display()))?;
  git(&["init", "-q"])?;
  std::fs::write(path.join(".gitignore"), "# scsh scratch — results, logs, cache. Never tracked.\n/tmp\n.harness/\n")
    .map_err(|e| format!("could not write .gitignore: {e}"))?;
  std::fs::create_dir_all(path.join("tmp")).map_err(|e| format!("could not create tmp/: {e}"))?;
  git(&["add", ".gitignore"])?;
  git(&[
    "-c",
    &format!("user.name={}", crate::SCSH_COMMIT_NAME),
    "-c",
    &format!("user.email={}", crate::SCSH_COMMIT_EMAIL),
    "commit",
    "-qm",
    "Init setup-tests project.",
  ])
}

fn render_setup_batch_yaml(tests: &[SetupTestRequest]) -> String {
  let mut out = String::from(
    r#"# Generated by the Setup tab — do not edit by hand.
description: "Setup model probes from the session browser."
task: |
  Connectivity probe. Write the JSON object
  {"ok": true, "model": "<the model id you are running as>",
   "message": "connectivity ok - responding as <the model id you are running as>"}
  to the file named by the SCSH_RESULT environment variable, then stop. Do nothing else.
invocations:
"#,
  );
  for (i, t) in tests.iter().enumerate() {
    let route = format!(
      "{}-{}",
      t.harness.as_str(),
      t.model
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect::<String>()
    );
    let route = if route.is_empty() { format!("t{i}") } else { route };
    out.push_str(&format!(
      "  {route}:\n    harness: {}\n    model: {}\n",
      t.harness.as_str(),
      yaml_escape_scalar(&t.model)
    ));
    if let Some(ref e) = t.effort {
      out.push_str(&format!("    effort: {e}\n"));
    } else if t.harness == Harness::Codex {
      out.push_str("    effort: low\n");
    }
  }
  out
}

fn yaml_escape_scalar(s: &str) -> String {
  if s.chars().any(|c| c.is_whitespace() || ":#{}[],&*?|>!%@`'\"".contains(c)) {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
  } else {
    s.to_string()
  }
}

pub fn setup_batch_def_name() -> &'static str {
  SETUP_BATCH_DEF
}
