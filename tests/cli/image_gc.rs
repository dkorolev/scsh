//! CLI cleanup tests use private fake runtimes and never touch the host image store.

#[cfg(unix)]
mod unix {
  use std::os::unix::fs::PermissionsExt;
  use std::path::PathBuf;
  use std::process::{Command, Output};

  struct Fixture {
    /// Private runtime executable, inventory and deletion log; removed even on assertion failure.
    dir: PathBuf,
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.dir);
    }
  }

  impl Fixture {
    fn new(runtime: &str) -> Self {
      let dir = std::env::temp_dir().join(format!("scsh-image-gc-test-{}-{runtime}", std::process::id()));
      std::fs::create_dir(&dir).unwrap();
      let fixture = Self { dir };
      let script = r#"#!/bin/sh
printf '%s\n' "$*" >> calls
if [ -f unavailable ]; then echo 'runtime unavailable' >&2; exit 1; fi
case "$*" in
  'image ls '*) printf '%s\n' 'OLD_ID' 'NEW_ID' ;;
  'image inspect '*|'image list --format json') /bin/cat images.json ;;
  'ps --all --quiet --no-trunc') ;;
  'list --all --format json') printf '%s\n' '[]' ;;
  'image rm --no-prune OLD_ID'|'image delete scsh-retired:OLD_HEX') printf '%s\n' "$*" >> removed ;;
  *) echo "unexpected runtime command: $*" >&2; exit 2 ;;
esac
"#;
      let script = script
        .replace("OLD_ID", &format!("sha256:{}", "a".repeat(64)))
        .replace("NEW_ID", &format!("sha256:{}", "b".repeat(64)))
        .replace("OLD_HEX", &"a".repeat(64));
      let executable = fixture.dir.join(runtime);
      std::fs::write(&executable, script).unwrap();
      std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
      let image = |id: &str, tag: &str, owned: bool| {
        if runtime == "container" {
          format!(
            r#"{{
            "configuration": {{"name": "{tag}", "descriptor": {{"digest": "sha256:{id}"}}}},
            "variants": [{{"config": {{"config": {{"Labels": {{
              "scsh.generated": "{owned}", "scsh.build.fingerprint": "FP"
            }}}}}}}}]
          }}"#
          )
        } else {
          let tags = if tag.is_empty() { String::new() } else { format!("\"{tag}\"") };
          format!(
            r#"{{
            "Id": "sha256:{id}", "RepoTags": [{tags}], "RepoDigests": [],
            "Config": {{"Labels": {{"scsh.generated": "{owned}", "scsh.build.fingerprint": "FP"}}}}
          }}"#
          )
        }
        .replace("FP", &"f".repeat(64))
      };
      let old = if runtime == "container" { format!("scsh-retired:{}", "a".repeat(64)) } else { String::new() };
      let inventory = format!(
        "[{}, {}, {}]",
        image(&"a".repeat(64), &old, true),
        image(&"b".repeat(64), "scsh-claude:latest", true),
        image(&"c".repeat(64), "unrelated:latest", false)
      );
      std::fs::write(fixture.dir.join("images.json"), inventory).unwrap();
      fixture
    }

    fn run(&self, runtime: &str, args: &[&str]) -> Output {
      // Only the runtime stub is discoverable; it uses shell builtins and /bin/cat.
      Command::new(env!("CARGO_BIN_EXE_scsh"))
        .args(args)
        .current_dir(&self.dir)
        .env("PATH", &self.dir)
        .env("SCSH_RUNTIME", runtime)
        .output()
        .unwrap()
    }
  }

  #[test]
  fn preview_apply_and_runtime_failure_are_safe_for_each_backend() {
    for runtime in ["docker", "podman", "container"] {
      let fixture = Fixture::new(runtime);
      let preview = fixture.run(runtime, &["gc-images"]);
      assert!(preview.status.success(), "{preview:?}");
      let stdout = String::from_utf8(preview.stdout).unwrap();
      assert!(stdout.contains("\"ImageCleanup\"") && stdout.contains("\"applied\": false"), "{stdout}");
      assert!(
        stdout.contains(&"a".repeat(64)) && !stdout.contains(&"b".repeat(64)) && !stdout.contains(&"c".repeat(64))
      );
      assert!(!fixture.dir.join("removed").exists());

      let apply = fixture.run(runtime, &["gc-images", "--apply", "--json"]);
      assert!(apply.status.success(), "{apply:?}");
      assert!(String::from_utf8(apply.stdout).unwrap().contains("\"removed\": 1"));
      let removed = std::fs::read_to_string(fixture.dir.join("removed")).unwrap();
      assert_eq!(removed.lines().count(), 1);
      assert!(removed.contains(&"a".repeat(64)));
      assert!(!removed.contains("latest") && !removed.contains("--force"));

      std::fs::write(fixture.dir.join("unavailable"), "").unwrap();
      let failed = fixture.run(runtime, &["gc-images", "--apply"]);
      assert_eq!(failed.status.code(), Some(1));
      assert!(String::from_utf8(failed.stdout).unwrap().contains("\"Error\""));
      assert_eq!(std::fs::read_to_string(fixture.dir.join("removed")).unwrap(), removed);
    }
  }
}
