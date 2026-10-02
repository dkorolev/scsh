//! Image replacement cleanup. Ownership is proved by OCI labels, never a name prefix alone.
//! Apple needs a retained reference before retagging: its CLI cannot resolve an orphan by ID.

use std::collections::BTreeSet;
use std::process::Command;
use std::time::Duration;

use crate::json::{self, Value};
use crate::runtime;

const RETAINED: &str = "scsh-retired:";
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
  /// Full immutable runtime image ID (including sha256 prefix).
  pub id: String,
  /// Every tag/digest reference reported by the runtime; ordinary references protect images.
  pub references: Vec<String>,
  /// Both scsh labels must be present and valid before deletion is considered.
  owned: bool,
  /// Fingerprint used to verify that a successful build really replaced the requested tag.
  fingerprint: Option<String>,
}

fn field<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
  let mut value = value;
  for name in path {
    let Value::Object(fields) = value else { return None };
    value = &fields.iter().find(|(key, _)| key == name)?.1;
  }
  Some(value)
}

fn string(value: &Value) -> Option<&str> {
  if let Value::String(s) = value {
    Some(s)
  } else {
    None
  }
}

fn text<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
  field(value, path).and_then(string)
}

fn array(value: &Value) -> Result<&[Value], String> {
  if let Value::Array(items) = value {
    Ok(items)
  } else {
    Err("expected a runtime JSON array".into())
  }
}

fn digest(id: &str) -> Result<String, String> {
  let hex = id.strip_prefix("sha256:").unwrap_or(id);
  if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
    return Err("runtime returned an invalid image digest".into());
  }
  Ok(format!("sha256:{hex}"))
}

fn reference(s: &str) -> &str {
  s.strip_prefix("docker.io/library/").or_else(|| s.strip_prefix("localhost/")).unwrap_or(s)
}

fn retained(image: &Image) -> String {
  format!("{RETAINED}{}", image.id.trim_start_matches("sha256:"))
}

fn parse_images(runtime: &str, raw: &str) -> Result<Vec<Image>, String> {
  let value = json::parse(raw)?;
  let mut images: Vec<Image> = Vec::new();
  for item in array(&value)? {
    let (id, references, labels) = if runtime == "container" {
      let id = text(item, &["configuration", "descriptor", "digest"]).ok_or("missing Apple image digest")?;
      let name = text(item, &["configuration", "name"]).ok_or("missing Apple image reference")?;
      let variants = array(field(item, &["variants"]).ok_or("missing Apple image variants")?)?;
      // All platform variants must prove ownership; a mixed index is never ours to delete.
      let labels: Vec<_> = variants.iter().map(|v| field(v, &["config", "config", "Labels"])).collect();
      (id, vec![name.to_string()], labels)
    } else {
      let id = text(item, &["Id"]).ok_or("missing image Id")?;
      let mut refs = Vec::new();
      for key in ["RepoTags", "RepoDigests"] {
        match field(item, &[key]) {
          Some(Value::Null) => {}
          Some(value) => {
            for value in array(value)? {
              let s = string(value).ok_or("invalid image reference")?;
              if s != "<none>:<none>" && s != "<none>@<none>" {
                refs.push(s.to_string());
              }
            }
          }
          None => return Err(format!("missing {key}; cannot prove image is untagged")),
        }
      }
      (id, refs, vec![field(item, &["Config", "Labels"])])
    };
    let fingerprint =
      labels.first().and_then(|l| *l).and_then(|l| text(l, &[runtime::BUILD_FINGERPRINT_LABEL])).map(str::to_owned);
    let owned = !labels.is_empty()
      && labels.iter().all(|l| {
        l.is_some_and(|l| {
          text(l, &["scsh.generated"]) == Some("true")
            && text(l, &[runtime::BUILD_FINGERPRINT_LABEL]).is_some_and(|fp| digest(fp).is_ok())
        })
      });
    let id = digest(id)?;
    if let Some(existing) = images.iter_mut().find(|i| i.id == id) {
      existing.references.extend(references);
      existing.owned &= owned;
    } else {
      images.push(Image { id, references, owned, fingerprint });
    }
  }
  Ok(images)
}

fn execute(runtime: &str, args: &[&str]) -> Result<String, String> {
  if !matches!(runtime, "docker" | "podman" | "container") {
    return Err("unsupported image runtime".into());
  }
  let out = runtime::command_output(Command::new(runtime).args(args), DEADLINE)?;
  if !out.status.success() {
    return Err(format!("{runtime} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
  }
  String::from_utf8(out.stdout).map_err(|e| e.to_string())
}

type Run<'a> = dyn Fn(&str, &[&str]) -> Result<String, String> + 'a;

fn inventory(runtime: &str, run: &Run<'_>) -> Result<Vec<Image>, String> {
  if runtime == "container" {
    return parse_images(runtime, &run(runtime, &["image", "list", "--format", "json"])?);
  }
  let ids = run(
    runtime,
    &[
      "image",
      "ls",
      "--all",
      "--quiet",
      "--no-trunc",
      "--filter",
      "label=scsh.generated=true",
      "--filter",
      "label=scsh.build.fingerprint",
    ],
  )?;
  let ids: BTreeSet<_> = ids.split_whitespace().map(digest).collect::<Result<_, _>>()?;
  if ids.is_empty() {
    return Ok(Vec::new());
  }
  let mut args = vec!["image", "inspect"];
  args.extend(ids.iter().map(String::as_str));
  parse_images(runtime, &run(runtime, &args)?)
}

fn used_images(runtime: &str, run: &Run<'_>) -> Result<BTreeSet<String>, String> {
  let raw = if runtime == "container" {
    run(runtime, &["list", "--all", "--format", "json"])?
  } else {
    let ids = run(runtime, &["ps", "--all", "--quiet", "--no-trunc"])?;
    let ids: Vec<_> = ids.split_whitespace().collect();
    if ids.is_empty() {
      return Ok(BTreeSet::new());
    }
    if ids.iter().any(|id| !id.bytes().all(|b| b.is_ascii_hexdigit())) {
      return Err("invalid container ID in runtime inventory".into());
    }
    let mut args = vec!["container", "inspect"];
    args.extend(ids);
    run(runtime, &args)?
  };
  let value = json::parse(&raw)?;
  array(&value)?
    .iter()
    .map(|item| {
      let path: &[&str] =
        if runtime == "container" { &["configuration", "image", "descriptor", "digest"] } else { &["Image"] };
      digest(text(item, path).ok_or("missing container image identity; cleanup refused")?)
    })
    .collect()
}

fn candidate(image: &Image, used: &BTreeSet<String>) -> bool {
  image.owned && !used.contains(&image.id) && image.references.iter().all(|r| reference(r) == retained(image))
}

/// Capture the old immutable ID before building. Apple retains a reference so a failed build
/// or a busy image remains discoverable for the next gc-images pass. Inventory failure aborts
/// before a retag; guessing that an inaccessible image is absent would silently leak it.
pub fn before_build(runtime: &str, tag: &str) -> Result<Option<Image>, String> {
  before_build_with(runtime, tag, &execute)
}

fn before_build_with(runtime: &str, tag: &str, run: &Run<'_>) -> Result<Option<Image>, String> {
  let images = inventory(runtime, run)?;
  let old = images.iter().find(|i| i.references.iter().any(|r| reference(r) == tag)).cloned();
  if let Some(image) = &old {
    if runtime == "container" && image.owned {
      // Apple ClientImage._search is reference-only (verified 2026-10-02):
      // github.com/apple/container/blob/main/Sources/Services/ContainerAPIService/Client/ClientImage.swift
      // Retire this extra tag when the CLI can reliably resolve/delete a superseded immutable ID.
      let keep = retained(image);
      if images.iter().any(|i| i.id != image.id && i.references.iter().any(|r| reference(r) == keep)) {
        return Err(format!("retention reference {keep} belongs to another image; refusing to overwrite it"));
      }
      run(runtime, &["image", "tag", tag, &keep])?;
      let verified = inventory(runtime, run)?
        .into_iter()
        .any(|i| i.id == image.id && i.references.iter().any(|r| reference(r) == keep));
      if !verified {
        return Err("old image changed while retaining it; retry the build".into());
      }
    }
  }
  Ok(old)
}

/// Successful builds alone reach this boundary. Verify the new tag/fingerprint before touching
/// the captured ID. Failed builds leave their retained reference protected by the current tag.
pub fn after_build(runtime: &str, tag: &str, fingerprint: &str, old: Option<&Image>) -> Result<(), String> {
  after_build_with(runtime, tag, fingerprint, old, &execute)
}

fn after_build_with(
  runtime: &str, tag: &str, fingerprint: &str, old: Option<&Image>, run: &Run<'_>,
) -> Result<(), String> {
  let images = inventory(runtime, run)?;
  let new = images
    .iter()
    .find(|i| i.references.iter().any(|r| reference(r) == tag))
    .ok_or("successful build has no inspectable image; cleanup deferred")?;
  if !new.owned || new.fingerprint.as_deref() != Some(fingerprint) {
    return Err("built image fingerprint was not confirmed; cleanup deferred".into());
  }
  if let Some(old) = old.filter(|old| old.owned && old.id != new.id) {
    if !remove(runtime, &old.id, run)? {
      return Err(format!("{} is still referenced or no longer eligible", old.id));
    }
  }
  Ok(())
}

/// Every deletion re-reads labels, references and *all* containers, including stopped ones.
/// Never force deletion, prune other images/parents, or delete by the mutable latest tag.
fn remove(runtime: &str, id: &str, run: &Run<'_>) -> Result<bool, String> {
  let images = inventory(runtime, run)?;
  let Some(image) = images.iter().find(|i| i.id == id) else { return Ok(false) };
  if !candidate(image, &used_images(runtime, run)?) {
    return Ok(false);
  }
  if runtime == "container" {
    run(runtime, &["image", "delete", &retained(image)])?;
  } else {
    run(runtime, &["image", "rm", "--no-prune", &image.id])?;
  }
  Ok(true)
}

/// Label-filtered plan; a failed/unknown inventory never produces deletion candidates.
pub fn plan(runtime: &str) -> Result<Vec<Image>, String> {
  let images = inventory(runtime, &execute)?;
  let used = used_images(runtime, &execute)?;
  Ok(images.into_iter().filter(|image| candidate(image, &used)).collect())
}

/// Apply only the immutable IDs shown in the plan, rechecking eligibility before each delete.
pub fn apply(runtime: &str, images: &[Image]) -> (usize, Vec<String>) {
  let mut removed = 0;
  let mut warnings = Vec::new();
  for image in images {
    match remove(runtime, &image.id, &execute) {
      Ok(true) => removed += 1,
      Ok(false) => warnings.push(format!("{} retained: now referenced or no longer eligible", image.id)),
      Err(e) => warnings.push(format!("{} retained: {e}", image.id)),
    }
  }
  (removed, warnings)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::RefCell;

  fn id(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
  }

  fn docker_image(c: char, tags: &[&str], owned: bool) -> Value {
    json::parse(&format!(
      r#"{{
      "Id": {},
      "RepoTags": [{}],
      "RepoDigests": [],
      "Config": {{"Labels": {{"scsh.generated": "{}", "scsh.build.fingerprint": "{}"}}}}
    }}"#,
      json::quote(&id(c)),
      tags.iter().map(|s| json::quote(s)).collect::<Vec<_>>().join(","),
      owned,
      "f".repeat(64)
    ))
    .unwrap()
  }

  fn apple_image(c: char, tag: &str, owned: bool) -> Value {
    json::parse(&format!(
      r#"{{
      "configuration": {{"name": {}, "descriptor": {{"digest": {}}}}},
      "variants": [{{"config": {{"config": {{"Labels": {{
        "scsh.generated": "{}", "scsh.build.fingerprint": "{}"
      }}}}}}}}]
    }}"#,
      json::quote(tag),
      json::quote(&id(c)),
      owned,
      "f".repeat(64)
    ))
    .unwrap()
  }

  fn inventory_json(runtime: &str, old_tag: Option<&str>) -> String {
    let items = if runtime == "container" {
      vec![
        apple_image('a', old_tag.unwrap_or(&format!("{RETAINED}{}", "a".repeat(64))), true),
        apple_image('b', "scsh-claude:latest", true),
      ]
    } else {
      vec![
        docker_image('a', &old_tag.into_iter().collect::<Vec<_>>(), true),
        docker_image(
          'b',
          &[if runtime == "podman" { "localhost/scsh-claude:latest" } else { "scsh-claude:latest" }],
          true,
        ),
      ]
    };
    json::write_pretty(&Value::Array(items))
  }

  #[test]
  fn only_owned_untagged_unused_images_are_candidates() {
    let images = parse_images(
      "docker",
      &json::write_pretty(&Value::Array(vec![
        docker_image('a', &[], true),
        docker_image('b', &["scsh-claude:latest"], true),
        docker_image('c', &["my-app:saved"], true),
        docker_image('d', &[], false),
        docker_image('e', &[], true),
      ])),
    )
    .unwrap();
    let used = BTreeSet::from([id('e')]);
    assert_eq!(images.iter().filter(|i| candidate(i, &used)).map(|i| i.id.clone()).collect::<Vec<_>>(), [id('a')]);
  }

  #[test]
  fn apple_aliases_merge_and_current_or_external_tags_protect_retained_images() {
    for tag in ["scsh-claude:latest", "my-app:saved"] {
      let raw = json::write_pretty(&Value::Array(vec![
        apple_image('a', &format!("docker.io/library/{RETAINED}{}", "a".repeat(64)), true),
        apple_image('a', tag, true),
      ]));
      let images = parse_images("container", &raw).unwrap();
      assert_eq!(images.len(), 1);
      assert!(!candidate(&images[0], &BTreeSet::new()));
    }
    let images = parse_images("container", &inventory_json("container", None)).unwrap();
    assert!(candidate(&images[0], &BTreeSet::new()));
    assert!(!candidate(&images[0], &BTreeSet::from([id('a')])));
  }

  #[test]
  fn malformed_or_incomplete_inventory_never_authorizes_deletion() {
    for raw in ["", "{}", "null", r#"[{"Id":"sha256:bad"}]"#] {
      assert!(parse_images("docker", raw).is_err());
      assert!(parse_images("container", raw).is_err());
    }
    let mut image = docker_image('a', &[], true);
    if let Value::Object(fields) = &mut image {
      fields.retain(|(key, _)| key != "RepoTags");
    }
    assert!(parse_images("docker", &json::write_pretty(&Value::Array(vec![image]))).is_err());
  }

  #[test]
  fn replacement_deletes_only_old_identity_without_force_or_broad_pruning() {
    for runtime in ["docker", "podman", "container"] {
      let calls = RefCell::new(Vec::new());
      let run = |_: &str, args: &[&str]| {
        calls.borrow_mut().push(args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        match args {
          ["image", "ls", ..] => Ok(format!("{}\n{}", id('a'), id('b'))),
          ["image", "inspect", ..] | ["image", "list", ..] => Ok(inventory_json(runtime, None)),
          ["ps", ..] => Ok(String::new()),
          ["list", ..] => Ok("[]".into()),
          ["image", "rm", "--no-prune", old] if *old == id('a') => Ok(String::new()),
          ["image", "delete", old] if *old == format!("{RETAINED}{}", "a".repeat(64)) => Ok(String::new()),
          _ => panic!("unexpected command {args:?}"),
        }
      };
      let old = Image { id: id('a'), references: vec!["scsh-claude:latest".into()], owned: true, fingerprint: None };
      after_build_with(runtime, "scsh-claude:latest", &"f".repeat(64), Some(&old), &run).unwrap();
      let calls = calls.borrow();
      assert_eq!(calls.iter().filter(|args| args.get(1).is_some_and(|a| a == "rm" || a == "delete")).count(), 1);
      assert!(!calls.iter().flatten().any(|a| a == "--force" || a == "prune"));
      if runtime != "container" {
        assert!(calls.iter().any(|args| args.contains(&"label=scsh.generated=true".into())
          && args.contains(&"label=scsh.build.fingerprint".into())));
      }
    }
  }

  #[test]
  fn changed_tags_busy_images_and_failed_inspection_are_never_deleted() {
    for runtime in ["docker", "podman", "container"] {
      for scenario in ["tagged", "busy", "unknown"] {
        let run = |_: &str, args: &[&str]| match args {
          ["image", "ls", ..] => Ok(id('a')),
          ["image", "inspect", ..] | ["image", "list", ..] => {
            Ok(inventory_json(runtime, (scenario == "tagged").then_some("my-app:keep")))
          }
          ["ps", ..] if scenario == "unknown" => Err("runtime offline".into()),
          ["ps", ..] => Ok("c".repeat(64)),
          ["container", "inspect", ..] => Ok(format!(r#"[{{"Image":{}}}]"#, json::quote(&id('a')))),
          ["list", ..] if scenario == "unknown" => Err("runtime offline".into()),
          ["list", ..] => {
            Ok(format!(r#"[{{"configuration":{{"image":{{"descriptor":{{"digest":{}}}}}}}}}]"#, json::quote(&id('a'))))
          }
          _ => panic!("must not delete: {args:?}"),
        };
        let result = remove(runtime, &id('a'), &run);
        if scenario == "unknown" {
          assert!(result.is_err());
        } else {
          assert_eq!(result, Ok(false));
        }
      }
    }
  }

  #[test]
  fn fingerprint_mismatch_and_unchanged_image_skip_deletion() {
    for fp in ["wrong".to_string(), "f".repeat(64)] {
      let old = Image { id: id('b'), references: vec![], owned: true, fingerprint: None };
      let run = |_: &str, args: &[&str]| match args {
        ["image", "list", ..] => Ok(inventory_json("container", None)),
        _ => panic!("must not delete: {args:?}"),
      };
      let result = after_build_with("container", "scsh-claude:latest", &fp, Some(&old), &run);
      assert_eq!(result.is_ok(), fp != "wrong");
    }
  }

  #[test]
  fn apple_retains_before_retag_and_failed_build_keeps_current_image() {
    let tagged = RefCell::new(false);
    let run = |_: &str, args: &[&str]| match args {
      ["image", "list", ..] => {
        let mut items = vec![apple_image('a', "scsh-claude:latest", true)];
        if *tagged.borrow() {
          items.push(apple_image('a', &format!("{RETAINED}{}", "a".repeat(64)), true));
        }
        Ok(json::write_pretty(&Value::Array(items)))
      }
      ["image", "tag", "scsh-claude:latest", target] => {
        assert_eq!(*target, format!("{RETAINED}{}", "a".repeat(64)));
        *tagged.borrow_mut() = true;
        Ok(String::new())
      }
      ["list", ..] => Ok("[]".into()),
      _ => panic!("failed build must not delete: {args:?}"),
    };
    assert_eq!(before_build_with("container", "scsh-claude:latest", &run).unwrap().unwrap().id, id('a'));
    assert!(*tagged.borrow());
    assert_eq!(remove("container", &id('a'), &run), Ok(false));
  }

  #[test]
  fn retention_name_collision_never_overwrites_another_image() {
    let run = |_: &str, args: &[&str]| match args {
      ["image", "list", ..] => Ok(json::write_pretty(&Value::Array(vec![
        apple_image('a', "scsh-claude:latest", true),
        apple_image('b', &format!("{RETAINED}{}", "a".repeat(64)), false),
      ]))),
      _ => panic!("must not overwrite retention reference: {args:?}"),
    };
    assert!(before_build_with("container", "scsh-claude:latest", &run).unwrap_err().contains("refusing to overwrite"));
  }
}
