//! Container-engine liveness and "it's installed but not running" advice.
//!
//! scsh's preflight already locates a runtime *binary* on `$PATH`; this adds the
//! second question a real run needs answered — *is the engine actually up?* — and,
//! when it isn't, the exact command to start it. The decision logic
//! ([`start_command`]) is pure and unit-tested; only [`is_running`] shells out.
//!
//! Everything is keyed by the runtime's name string (`"docker"` / `"podman"` /
//! `"container"`), the same identifier [`crate::runtime::Runtime`] already carries,
//! so there is no parallel runtime enum to keep in sync.

use std::process::{Command, Stdio};

/// The host operating system, as far as the start-command advice cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
  Mac,
  Linux,
  Other,
}

impl Os {
  /// The OS this binary was compiled for.
  pub fn current() -> Os {
    if cfg!(target_os = "macos") {
      Os::Mac
    } else if cfg!(target_os = "linux") {
      Os::Linux
    } else {
      Os::Other
    }
  }
}

/// Human-friendly name for messages (falls back to the raw name for an
/// `SCSH_RUNTIME` scsh doesn't have canned advice for).
pub fn display_name(runtime: &str) -> String {
  match runtime {
    "docker" => "Docker".to_string(),
    "podman" => "Podman".to_string(),
    "container" => "Apple container".to_string(),
    other => other.to_string(),
  }
}

/// Arguments to a cheap "are you actually up?" probe for the engine.
///
/// `info` talks to the docker/podman daemon and fails fast if it isn't reachable;
/// Apple's `container list` fails until its system service is started. Unknown
/// runtimes get `info`, the near-universal convention.
fn liveness_probe(runtime: &str) -> &'static [&'static str] {
  match runtime {
    "container" => &["list"],
    _ => &["info"],
  }
}

/// The command that starts the engine, for the given OS — `None` when scsh has no
/// canned advice for this runtime name. Best-effort and documented as an
/// assumption in the README.
pub fn start_command(runtime: &str, os: Os) -> Option<String> {
  let cmd = match (runtime, os) {
    ("docker", Os::Mac) => "open -a Docker",
    ("docker", Os::Linux) => "sudo systemctl start docker",
    ("docker", Os::Other) => "start Docker Desktop",

    ("podman", Os::Mac) => "podman machine start",
    ("podman", Os::Linux) => "systemctl --user start podman.socket",
    ("podman", Os::Other) => "podman machine start",

    // Apple's `container` only exists on macOS, but answer sensibly regardless.
    ("container", _) => "container system start",

    _ => return None,
  };
  Some(cmd.to_string())
}

/// Is the engine up and accepting work? Runs the runtime's liveness probe
/// (`<runtime> info`, or `container list`) quietly and reports whether it
/// succeeded. A missing binary or a probe error both read as "not running".
pub fn is_running(runtime: &str) -> bool {
  Command::new(runtime)
    .args(liveness_probe(runtime))
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn display_names_are_friendly_with_a_raw_fallback() {
    assert_eq!(display_name("docker"), "Docker");
    assert_eq!(display_name("podman"), "Podman");
    assert_eq!(display_name("container"), "Apple container");
    assert_eq!(display_name("nerdctl"), "nerdctl");
  }

  #[test]
  fn start_commands_are_os_specific() {
    assert_eq!(start_command("docker", Os::Mac).as_deref(), Some("open -a Docker"));
    assert!(start_command("docker", Os::Linux).unwrap().contains("systemctl start docker"));
    assert_eq!(start_command("podman", Os::Mac).as_deref(), Some("podman machine start"));
    assert!(start_command("podman", Os::Linux).unwrap().contains("podman.socket"));
    assert_eq!(start_command("container", Os::Mac).as_deref(), Some("container system start"));
    // Unknown runtimes have no canned start command.
    assert_eq!(start_command("nerdctl", Os::Linux), None);
  }
}
