//! Setup tab payload: per-harness image + login readiness (and curated models as not-tested).
//!
//! `GET /api/v1/setup` composes existing image inspect + host login preflight into a
//! browser-safe JSON shape. No secrets, no automatic model calls.

use crate::config::Harness;
use crate::json::quote;
use crate::runtime::{self, ImageStatus, LoginPreflight};

/// One curated smoke model per harness (aligned with the built-in `doctor` definition).
pub struct CatalogModel {
  pub id: &'static str,
  pub kind: &'static str, // "primary"
}

/// Primary smoke models shared with `doctor` — Phase 2 catalog seed.
pub fn primary_models(harness: Harness) -> &'static [CatalogModel] {
  match harness {
    Harness::Opencode => &[CatalogModel { id: "openai/gpt-5.5", kind: "primary" }],
    Harness::Claude => &[CatalogModel { id: "claude-opus-4-8", kind: "primary" }],
    Harness::Codex => &[CatalogModel { id: "gpt-5.5", kind: "primary" }],
    Harness::Grok => &[CatalogModel { id: "grok-build", kind: "primary" }],
    Harness::Cursor => &[CatalogModel { id: "composer-2.5-fast", kind: "primary" }],
  }
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
    s.created.as_deref().map(|v| quote(v)).unwrap_or_else(|| "null".into()),
    s.size.as_deref().map(|v| quote(v)).unwrap_or_else(|| "null".into()),
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
  let parts: Vec<String> = primary_models(harness)
    .iter()
    .map(|m| format!("{{ \"id\": {}, \"kind\": {}, \"status\": \"not_tested\" }}", quote(m.id), quote(m.kind),))
    .collect();
  format!("[{}]", parts.join(", "))
}

/// Overall harness readiness for Phase 1 (no end-to-end model probes yet).
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
    "not_tested" => "Not tested",
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
      r#"{ "kind": "none", "label": "", "hint": "Image and login look ready — model tests are not run automatically" }"#
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
  use crate::config::Harness;

  #[test]
  fn primary_models_cover_every_harness() {
    for h in Harness::ALL {
      assert!(!primary_models(h).is_empty(), "{} needs a primary model", h.as_str());
    }
  }

  #[test]
  fn setup_json_error_when_runtime_unknown() {
    let j = setup_json(Some("not-a-runtime-xyz"));
    assert!(j.contains("\"error\""), "{j}");
    assert!(j.contains("not-a-runtime-xyz"), "{j}");
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
}
