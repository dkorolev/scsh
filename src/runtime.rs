//! Container-runtime discovery and the in-memory build/run plan.
//!
//! Everything here is pure and side-effect-free except [`which`] / [`detect_runtime`],
//! which only read `$PATH`. The actual process spawning lives in `main.rs`, which
//! keeps this module easy to unit-test without a container runtime installed.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::config::Harness;

/// A located container runtime executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
  pub name: String,
  pub path: PathBuf,
}

/// Candidate runtimes, in the order scsh tries them. Apple's `container` is
/// preferred on macOS; Docker is the primary everywhere; Podman is the fallback.
pub fn runtime_candidates(is_macos: bool) -> &'static [&'static str] {
  if is_macos {
    &["container", "docker", "podman"]
  } else {
    &["docker", "podman"]
  }
}

/// Find the first available runtime for the current OS. If `SCSH_RUNTIME` is set
/// (and non-empty), it overrides detection — scsh uses exactly that runtime when
/// it is on `PATH`. This is handy when the auto-picked runtime can't bind-mount
/// the clone (e.g. snap-packaged Docker is confined away from `/tmp`, so a
/// `SCSH_RUNTIME=podman` override is needed there).
pub fn detect_runtime() -> Option<Runtime> {
  if let Some(name) = std::env::var_os("SCSH_RUNTIME") {
    let name = name.to_string_lossy().into_owned();
    if !name.is_empty() {
      return which(&name).map(|path| Runtime { name, path });
    }
  }
  let path = std::env::var_os("PATH").unwrap_or_default();
  detect_runtime_in(cfg!(target_os = "macos"), &path)
}

/// Testable core of [`detect_runtime`]: search `path` for the OS's candidates.
///
/// Auto-detection additionally avoids a **snap-packaged Docker**: it is
/// AppArmor-confined away from the system temp dir, so it can't bind-mount the
/// per-run clone (the container would see an empty `/home/agent` and the skill's
/// opencode would crash with `EACCES`). When the preferred runtime is a snap
/// Docker *and* another runtime is available, scsh picks the other one instead.
/// An explicit `SCSH_RUNTIME` still forces any choice (see [`detect_runtime`]).
pub fn detect_runtime_in(is_macos: bool, path: &OsStr) -> Option<Runtime> {
  let found: Vec<Runtime> = runtime_candidates(is_macos)
    .iter()
    .filter_map(|&name| which_in(name, path).map(|p| Runtime { name: name.to_string(), path: p }))
    .collect();
  let snap_docker_first = matches!(found.first(), Some(r) if r.name == "docker" && is_snap_confined(&r.path));
  if snap_docker_first {
    if let Some(other) = found.iter().find(|r| r.name != "docker") {
      return Some(other.clone());
    }
  }
  found.into_iter().next()
}

/// Whether an executable path is inside a snap mount (e.g. `/snap/bin/docker`).
/// Snap-packaged Docker can't reach the system temp dir, which is where scsh
/// puts each run's clone, so the container sees nothing mounted.
pub fn is_snap_confined(path: &Path) -> bool {
  path.to_string_lossy().contains("/snap/")
}

/// Resolve an executable on `$PATH` (like the `which` command).
pub fn which(cmd: &str) -> Option<PathBuf> {
  let path = std::env::var_os("PATH")?;
  which_in(cmd, &path)
}

/// Testable core of [`which`]: search the given `path` value.
pub fn which_in(cmd: &str, path: &OsStr) -> Option<PathBuf> {
  if cmd.contains('/') {
    let p = PathBuf::from(cmd);
    return is_executable(&p).then_some(p);
  }
  for dir in std::env::split_paths(path) {
    if dir.as_os_str().is_empty() {
      continue;
    }
    let candidate = dir.join(cmd);
    if is_executable(&candidate) {
      return Some(candidate);
    }
  }
  None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
  use std::os::unix::fs::PermissionsExt;
  match std::fs::metadata(p) {
    Ok(m) => m.is_file() && (m.permissions().mode() & 0o111 != 0),
    Err(_) => false,
  }
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
  std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

/// The tag of the harness-specific image scsh builds.
pub fn image_tag(harness: Harness) -> String {
  match harness {
    Harness::Opencode => "scsh-opencode:latest".to_string(),
    Harness::Claude => "scsh-claude:latest".to_string(),
  }
}

/// The Dockerfile build `--target` for a harness image.
pub fn image_target(harness: Harness) -> &'static str {
  match harness {
    Harness::Opencode => "scsh-opencode",
    Harness::Claude => "scsh-claude",
  }
}

/// In-container path where the host's Claude Code config dir is bind-mounted.
pub const CLAUDE_CONFIG_MOUNT: &str = "/home/agent/.claude";

/// In-container path for the forwarded `~/.claude.json`.
pub const CLAUDE_JSON_MOUNT: &str = "/home/agent/.claude.json";

/// Run-dir-relative path where scsh copies forwarded Claude auth before a run (gitignored `tmp/`).
pub const CLAUDE_AUTH_REL: &str = "tmp/.claude-auth";

/// In-container path where opencode reads `auth.json` (`$XDG_DATA_HOME/opencode/auth.json` in the
/// image). scsh bind-mounts the host's `~/.local/share/opencode/auth.json` here when that file
/// exists — required for third-party opencode providers (e.g. Nebius GLM) that authenticate via
/// the host login rather than a built-in model route.
pub const OPENCODE_AUTH_MOUNT: &str = "/home/agent/repo/tmp/.xdg-data/opencode/auth.json";

/// Run-dir-relative tree where scsh copies forwarded opencode auth/config before a run.
pub const OPENCODE_FORWARD_REL: &str = "tmp/.opencode-forward";

/// In-container paths for forwarded opencode config (`$XDG_CONFIG_HOME/opencode/` on the host).
/// Custom providers (e.g. Nebius GLM) are declared here; auth.json alone is not enough.
/// scsh copies these from the host into each run clone (parallel runs cannot safely share one
/// host bind-mount on Apple Containers).
pub const OPENCODE_CONFIG_JSON_MOUNT: &str = "/home/agent/.config/opencode/opencode.json";
pub const OPENCODE_CONFIG_JSONC_MOUNT: &str = "/home/agent/.config/opencode/opencode.jsonc";

/// Host env var for long-lived Claude OAuth (`claude setup-token`).
pub const CLAUDE_OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Absolute path the repo clone is bind-mounted at, and the image's WORKDIR (where the harness
/// starts). Deliberately a *subdirectory* of the agent user's home (`/home/agent`), not the
/// home itself: the harness and its tools scribble into `$HOME` (`~/.cache`, `~/.config`,
/// `~/.npm`, …), so keeping the clone one level down keeps that scratch out of the repo's
/// working tree. The home is set in the image (see `src/Dockerfile`).
pub const AGENT_REPO: &str = "/home/agent/repo";

/// opencode's data dir (`XDG_DATA_HOME`), RELATIVE to the repo, where scsh drops the forwarded
/// credential. It lives under the gitignored `tmp/`, so neither the auth nor opencode's own
/// session data ever shows up as an untracked file. The image sets `XDG_DATA_HOME` to
/// [`AGENT_REPO`]`/`this.
pub const AGENT_XDG_DATA_REL: &str = "tmp/.xdg-data";

/// Per-run log path the harness tees every line of its output to, RELATIVE to the repo. It
/// lives under the gitignored `tmp/` (so it is never an untracked file); on the host it is
/// therefore `<run_dir>/tmp/scsh-run.log`, where the full intra-container output can be read.
pub const RUN_LOG_REL: &str = "tmp/scsh-run.log";

/// The env var (set in the generated image) that carries the in-container log
/// path the harness command tees its output to.
pub const RUN_LOG_VAR: &str = "SCSH_RUN_LOG";

/// The Dockerfile scsh builds every skill container from. The source of truth is the
/// sibling [`src/Dockerfile`](./Dockerfile) — a static, platform-agnostic file embedded at
/// compile time. It needs no Rust-side substitution: UID/GID/TZ are `ARG`s passed as build
/// args, and every architecture-specific download resolves the target arch *inside* the
/// build (`dpkg --print-architecture` -> amd64|arm64), so the one file builds on x86_64 and
/// arm64 alike. The image is generic (opencode + a dev toolchain + a non-root `agent` user,
/// no skill `CMD`), so it serves every skill; `main.rs` streams it to the builder's stdin.
pub fn dockerfile() -> String {
  include_str!("Dockerfile").to_string()
}

/// The builder host's IANA timezone (e.g. `Europe/Berlin`), baked into the image so a skill's
/// timestamps match the machine that built it. Tries `$TZ`, then the `/etc/localtime` symlink
/// target, then `/etc/timezone`; falls back to `UTC`.
pub fn host_timezone() -> String {
  if let Ok(tz) = std::env::var("TZ") {
    let tz = tz.trim();
    if !tz.is_empty() {
      return tz.to_string();
    }
  }
  if let Ok(target) = std::fs::read_link("/etc/localtime") {
    let s = target.to_string_lossy();
    if let Some(idx) = s.find("zoneinfo/") {
      let tz = s[idx + "zoneinfo/".len()..].trim_matches('/');
      if !tz.is_empty() {
        return tz.to_string();
      }
    }
  }
  if let Ok(contents) = std::fs::read_to_string("/etc/timezone") {
    let tz = contents.trim();
    if !tz.is_empty() {
      return tz.to_string();
    }
  }
  "UTC".to_string()
}

/// The shell command a harness runs *inside the container* for one skill.
pub fn harness_command(harness: Harness, model: Option<&str>, skill_source: &str, result: &str) -> String {
  match harness {
    Harness::Opencode => {
      let mut cmd = String::from("opencode");
      if let Some(m) = model {
        cmd.push(' ');
        cmd.push_str("-m ");
        cmd.push_str(&shell_quote(m));
      }
      cmd.push_str(" run ");
      cmd.push_str(&shell_quote(&format!("run skill {skill_source}")));
      format!("{cmd} 2>&1 | tee \"${RUN_LOG_VAR}\"")
    }
    Harness::Claude => {
      let prompt = format!(
        "Run the skill defined in .skills/{skill_source}/SKILL.md. Follow its instructions exactly. \
         Write the required result file to {result} (also available as the SCSH_RESULT environment variable)."
      );
      let mut cmd = String::from("claude -p ");
      cmd.push_str(&shell_quote(&prompt));
      cmd.push_str(" --permission-mode bypassPermissions --no-session-persistence");
      if let Some(m) = model {
        cmd.push_str(" --model ");
        cmd.push_str(&shell_quote(m));
      }
      format!("{cmd} 2>&1 | tee \"${RUN_LOG_VAR}\"")
    }
  }
}

/// How a given runtime accepts the generated Dockerfile.
///
/// docker and podman read it from stdin (`build … -`), which keeps it fully
/// in-memory and dodges build-context path confinement (e.g. snap-packaged
/// Docker can't read `/tmp`). Apple's `container` has no stdin build mode — it
/// requires a context directory — so for it scsh writes the in-memory Dockerfile
/// to an ephemeral context dir that is removed right after the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMethod {
  Stdin,
  ContextDir,
}

/// The filename scsh writes inside the ephemeral context dir (the universal
/// default Dockerfile name, so no `-f` flag is needed).
pub const CONTEXT_DOCKERFILE_NAME: &str = "Dockerfile";

/// Pick the build method for a runtime.
pub fn build_method(runtime: &str) -> BuildMethod {
  if runtime == "container" {
    BuildMethod::ContextDir
  } else {
    BuildMethod::Stdin
  }
}

/// The `--build-arg` pair that pins the agent's UID/GID to the host user's.
fn build_args(uid: u32, gid: u32, tz: &str) -> Vec<String> {
  vec![
    "--build-arg".into(),
    format!("AGENT_UID={uid}"),
    "--build-arg".into(),
    format!("AGENT_GID={gid}"),
    "--build-arg".into(),
    format!("TZ={tz}"),
  ]
}

/// Build argv for the stdin method: the Dockerfile is sent on stdin (`-`).
pub fn build_command_stdin(runtime: &str, tag: &str, target: &str, uid: u32, gid: u32, tz: &str) -> Vec<String> {
  let mut v = vec![runtime.into(), "build".into(), "-t".into(), tag.into(), "--target".into(), target.into()];
  v.extend(build_args(uid, gid, tz));
  v.push("-".into());
  v
}

pub fn build_command_context(
  runtime: &str, tag: &str, target: &str, context_dir: &str, uid: u32, gid: u32, tz: &str,
) -> Vec<String> {
  let mut v = vec![runtime.into(), "build".into(), "-t".into(), tag.into(), "--target".into(), target.into()];
  v.extend(build_args(uid, gid, tz));
  v.push(context_dir.into());
  v
}

/// Run argv: run the freshly built image with the repo clone bind-mounted at
/// [`AGENT_REPO`], removing the container afterwards. For rootless podman,
/// `--userns=keep-id` maps the host UID to the same UID inside the container so
/// the `agent` user can read/write the mount; docker (and Apple `container`) map
/// the UID directly and need no such flag.
pub fn run_command(
  runtime: &str,
  tag: &str,
  clone_dir: &str,
  name: &str,
  env: &[(String, String)],
  volumes: &[(&str, &str)],
  command: &str,
) -> Vec<String> {
  let mut v = vec![runtime.into(), "run".into(), "--rm".into(), "--name".into(), name.into()];
  if runtime == "podman" {
    v.push("--userns=keep-id".into());
  }
  for (key, value) in env {
    v.push("-e".into());
    v.push(format!("{key}={value}"));
  }
  for (host, mount) in volumes {
    v.push("-v".into());
    v.push(format!("{host}:{mount}"));
  }
  v.push("-v".into());
  v.push(format!("{clone_dir}:{AGENT_REPO}"));
  v.push(tag.into());
  v.push("/bin/sh".into());
  v.push("-c".into());
  v.push(command.into());
  v
}

pub fn opencode_auth_in(xdg_data_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
  let base = match xdg_data_home {
    Some(x) if !x.is_empty() => PathBuf::from(x),
    _ => PathBuf::from(home?).join(".local").join("share"),
  };
  Some(base.join("opencode").join("auth.json"))
}

/// Host opencode config dir (`$XDG_CONFIG_HOME/opencode` or `~/.config/opencode`).
pub fn opencode_config_dir(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
  let base = match xdg_config_home {
    Some(x) if !x.is_empty() => PathBuf::from(x),
    _ => PathBuf::from(home?).join(".config"),
  };
  Some(base.join("opencode"))
}

pub fn opencode_config_json_in(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
  let path = opencode_config_dir(xdg_config_home, home)?.join("opencode.json");
  path.is_file().then_some(path)
}

pub fn opencode_config_jsonc_in(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
  let path = opencode_config_dir(xdg_config_home, home)?.join("opencode.jsonc");
  path.is_file().then_some(path)
}

pub fn opencode_auth_ready() -> bool {
  opencode_auth_in(std::env::var_os("XDG_DATA_HOME").as_deref(), std::env::var_os("HOME").as_deref())
    .is_some_and(|p| p.is_file())
}

pub fn claude_oauth_token() -> Option<String> {
  std::env::var(CLAUDE_OAUTH_TOKEN_ENV).ok().filter(|s| !s.is_empty())
}

fn claude_credentials_file_on_host() -> Option<PathBuf> {
  let home = std::env::var_os("HOME")?;
  let path = PathBuf::from(home).join(".claude").join(".credentials.json");
  path.is_file().then_some(path)
}

/// Whether the host has credentials containers can use: `CLAUDE_CODE_OAUTH_TOKEN` or
/// `~/.claude/.credentials.json`.
pub fn claude_container_auth_ready() -> bool {
  claude_oauth_token().is_some() || claude_credentials_file_on_host().is_some()
}

pub fn check_harness_host(harness: Harness) -> Result<(), String> {
  match harness {
    Harness::Opencode => {
      if opencode_auth_ready() {
        Ok(())
      } else {
        Err("opencode auth not found (~/.local/share/opencode/auth.json) — run `opencode auth login`".into())
      }
    }
    Harness::Claude => {
      if claude_container_auth_ready() {
        Ok(())
      } else {
        Err(
          "CLAUDE_CODE_OAUTH_TOKEN is not set and ~/.claude/.credentials.json was not found \
           — run `claude setup-token`, then export CLAUDE_CODE_OAUTH_TOKEN in your shell"
            .into(),
        )
      }
    }
  }
}

/// Host opencode paths for `scsh list --verbose` (real runs copy into the run clone first).
pub fn opencode_host_mounts() -> Vec<(String, String)> {
  opencode_host_mounts_from(
    std::env::var_os("XDG_DATA_HOME").as_deref(),
    std::env::var_os("XDG_CONFIG_HOME").as_deref(),
    std::env::var_os("HOME").as_deref(),
  )
}

pub fn opencode_host_mounts_from(
  xdg_data_home: Option<&OsStr>,
  xdg_config_home: Option<&OsStr>,
  home: Option<&OsStr>,
) -> Vec<(String, String)> {
  let mut out = Vec::new();
  if let Some(auth) = opencode_auth_in(xdg_data_home, home).filter(|p| p.is_file()) {
    out.push((auth.to_string_lossy().into_owned(), OPENCODE_AUTH_MOUNT.to_string()));
  }
  if let Some(cfg) = opencode_config_json_in(xdg_config_home, home) {
    out.push((cfg.to_string_lossy().into_owned(), OPENCODE_CONFIG_JSON_MOUNT.to_string()));
  }
  if let Some(cfg) = opencode_config_jsonc_in(xdg_config_home, home) {
    out.push((cfg.to_string_lossy().into_owned(), OPENCODE_CONFIG_JSONC_MOUNT.to_string()));
  }
  out
}

/// Bind-mount opencode auth/config copied into a run clone.
pub fn opencode_forward_mounts(forward_root: &Path) -> Vec<(String, String)> {
  let mut out = Vec::new();
  let auth = forward_root.join("xdg/opencode/auth.json");
  if auth.is_file() {
    out.push((auth.to_string_lossy().into_owned(), OPENCODE_AUTH_MOUNT.to_string()));
  }
  let json = forward_root.join("config/opencode/opencode.json");
  if json.is_file() {
    out.push((json.to_string_lossy().into_owned(), OPENCODE_CONFIG_JSON_MOUNT.to_string()));
  }
  let jsonc = forward_root.join("config/opencode/opencode.jsonc");
  if jsonc.is_file() {
    out.push((jsonc.to_string_lossy().into_owned(), OPENCODE_CONFIG_JSONC_MOUNT.to_string()));
  }
  out
}

/// Volume mounts for forwarded Claude auth copied into `auth_root` (under a run dir).
pub fn claude_auth_mounts(auth_root: &Path) -> Vec<(String, String)> {
  let mut out = Vec::new();
  let claude_dir = auth_root.join(".claude");
  if claude_dir.is_dir() {
    out.push((claude_dir.to_string_lossy().into_owned(), CLAUDE_CONFIG_MOUNT.to_string()));
  }
  let claude_json = auth_root.join(".claude.json");
  if claude_json.is_file() {
    out.push((claude_json.to_string_lossy().into_owned(), CLAUDE_JSON_MOUNT.to_string()));
  }
  out
}

/// Volume mounts shown by `scsh list --verbose` (host paths; real runs use the same bind-mounts).
pub fn harness_volumes(harness: Harness) -> Vec<(String, String)> {
  match harness {
    Harness::Opencode => opencode_host_mounts(),
    Harness::Claude => {
      let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
      };
      let home = PathBuf::from(home);
      let mut out = Vec::new();
      let claude_dir = home.join(".claude");
      if claude_dir.is_dir() {
        out.push((claude_dir.to_string_lossy().into_owned(), CLAUDE_CONFIG_MOUNT.to_string()));
      }
      let claude_json = home.join(".claude.json");
      if claude_json.is_file() {
        out.push((claude_json.to_string_lossy().into_owned(), CLAUDE_JSON_MOUNT.to_string()));
      }
      out
    }
  }
}

/// Render an argv as a copy-pasteable shell command (for `scsh list --verbose`).
pub fn shell_join(args: &[String]) -> String {
  args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

fn shell_quote(s: &str) -> String {
  let safe = !s.is_empty()
    && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '=' | '+'));
  if safe {
    s.to_string()
  } else {
    format!("'{}'", s.replace('\'', r"'\''"))
  }
}

// ---------------------------------------------------------------------------
// UTC timestamps and the /tmp run-dir / backup names
// ---------------------------------------------------------------------------

/// Convert a count of days since 1970-01-01 to a `(year, month, day)` triple in
/// the proleptic Gregorian calendar — Howard Hinnant's `civil_from_days`. This
/// is what lets scsh format a UTC timestamp with only the standard library.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
  let z = z + 719_468;
  let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
  let doe = z - era * 146_097; // [0, 146096]
  let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
  let y = yoe + era * 400;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
  let mp = (5 * doy + 2) / 153; // [0, 11]
  let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
  let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
  (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Format Unix `epoch_secs` as a UTC `YYYYMMDD-HHMMSS` stamp (no separators
/// beyond the dash), matching scsh's run-dir and backup naming convention.
pub fn format_utc_timestamp(epoch_secs: u64) -> String {
  let days = (epoch_secs / 86_400) as i64;
  let tod = epoch_secs % 86_400;
  let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
  let (y, m, d) = civil_from_days(days);
  format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// Apple Containers (and Docker's normative pattern) cap container IDs at 64 characters.
pub const CONTAINER_ID_MAX_LEN: usize = 64;

/// Six lowercase `[a-z]` letters — the Apple-container run-dir stamp in place of UTC time.
pub fn random_nonce_6() -> String {
  let mut buf = [0u8; 6];
  let filled = std::fs::File::open("/dev/urandom")
    .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
    .is_ok();
  if !filled {
    let nanos = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_nanos())
      .unwrap_or(0) as u64;
    let seed = nanos ^ ((std::process::id() as u64) << 32);
    for (i, b) in buf.iter_mut().enumerate() {
      *b = ((seed.wrapping_mul(1_103_515_245).wrapping_add(i as u64)) % 26) as u8;
    }
  }
  buf.iter().map(|b| (b'a' + (b % 26)) as char).collect()
}

/// Shorten `s` to at most `max_len` by keeping the start and end with `..` in the middle.
pub fn truncate_middle(s: &str, max_len: usize) -> String {
  if max_len == 0 {
    return String::new();
  }
  if s.len() <= max_len {
    return s.to_string();
  }
  if max_len <= 2 {
    return s.chars().take(max_len).collect();
  }
  let keep = max_len - 2;
  let first_k = keep.div_ceil(2);
  let last_n = keep / 2;
  let bytes = s.as_bytes();
  let first = &s[..first_k];
  let last = std::str::from_utf8(&bytes[bytes.len() - last_n..]).unwrap_or("");
  format!("{first}..{last}")
}

fn apple_container_run_dir_name_with_nonce(skill: &str, nonce: &str) -> String {
  let prefix = format!("scsh-{nonce}-run-");
  let budget = CONTAINER_ID_MAX_LEN.saturating_sub(prefix.len());
  let skill_part = truncate_middle(skill, budget);
  format!("{prefix}{skill_part}")
}

/// Whether `name` looks like a per-run scratch dir under `/tmp` (UTC stamp or Apple nonce).
pub fn is_scsh_run_dir_name(name: &str) -> bool {
  if !name.starts_with("scsh-") {
    return false;
  }
  if name.contains("-utc-run-") {
    return true;
  }
  let rest = match name.strip_prefix("scsh-") {
    Some(r) => r,
    None => return false,
  };
  let (nonce, _) = match rest.split_once("-run-") {
    Some(pair) => pair,
    None => return false,
  };
  nonce.len() == 6 && nonce.chars().all(|c| c.is_ascii_lowercase())
}

/// Name of the per-run scratch directory created under `/tmp`.
///
/// Docker/podman: `scsh-YYYYMMDD-HHMMSS-utc-run-<skill>`.
/// Apple `container`: `scsh-<nonce>-run-<skill>` (≤ [`CONTAINER_ID_MAX_LEN`] chars; the skill
/// segment is middle-truncated with `..` when needed).
pub fn run_dir_name(epoch_secs: u64, skill: &str, runtime: &str) -> String {
  let skill = sanitize_component(skill);
  if runtime == "container" {
    apple_container_run_dir_name_with_nonce(&skill, &random_nonce_6())
  } else {
    format!("scsh-{}-utc-run-{}", format_utc_timestamp(epoch_secs), skill)
  }
}

/// Name an existing file is moved to before scsh overwrites it with a fresh
/// result: `<name>.bak.YYYYMMDD-HHMMSS-utc`.
pub fn backup_name(file_name: &str, epoch_secs: u64) -> String {
  format!("{file_name}.bak.{}-utc", format_utc_timestamp(epoch_secs))
}

/// Sanitize a skill name into a filesystem-safe path component (lowercased,
/// non-`[a-z0-9._-]` mapped to `-`, edges trimmed). Empty input becomes `skill`.
/// Also used for the `scsh/incoming/<skill>-…` branch names (the same charset is a
/// valid git ref component).
pub fn sanitize_component(s: &str) -> String {
  let mapped: String = s
    .chars()
    .map(|c| {
      let c = c.to_ascii_lowercase();
      if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
        c
      } else {
        '-'
      }
    })
    .collect();
  let trimmed = mapped.trim_matches(|c| matches!(c, '.' | '_' | '-'));
  if trimmed.is_empty() {
    "skill".to_string()
  } else {
    trimmed.to_string()
  }
}

// ---------------------------------------------------------------------------
// Cloning the host repo into the run dir
// ---------------------------------------------------------------------------

/// `git clone` argv that makes a full (deep, all-history) clone of the host repo
/// at `src` into `dst`. A local clone already fetches every branch into
/// `refs/remotes/origin/*`; scsh then materializes them as local branches (see
/// [`local_branches_to_create`]) so all branches are present in the container.
pub fn clone_command(src: &str, dst: &str) -> Vec<String> {
  vec!["git".into(), "clone".into(), src.into(), dst.into()]
}

/// Given the lines of `git for-each-ref --format='%(refname:short)'
/// refs/remotes/origin` and the clone's current branch, return the local branch
/// names to create so every remote branch becomes a local one. `origin/HEAD`
/// (the symbolic default pointer) and the already-checked-out branch are skipped.
pub fn local_branches_to_create(for_each_ref: &str, current_branch: &str) -> Vec<String> {
  let mut out = Vec::new();
  for line in for_each_ref.lines() {
    let line = line.trim();
    let branch = match line.strip_prefix("origin/") {
      Some(b) => b,
      None => continue,
    };
    if branch == "HEAD" || branch == current_branch || branch.is_empty() {
      continue;
    }
    if !out.iter().any(|b: &String| b == branch) {
      out.push(branch.to_string());
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::ffi::OsString;
  use std::sync::atomic::{AtomicUsize, Ordering};

  static COUNTER: AtomicUsize = AtomicUsize::new(0);

  fn tmp(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!("scsh-ut-{tag}-{}-{nanos}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
  }

  #[cfg(unix)]
  fn make_exec(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, "#!/bin/sh\n").unwrap();
    let mut perms = std::fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(p, perms).unwrap();
  }

  #[test]
  fn candidates_depend_on_os() {
    assert_eq!(runtime_candidates(true), &["container", "docker", "podman"]);
    assert_eq!(runtime_candidates(false), &["docker", "podman"]);
  }

  #[cfg(unix)]
  #[test]
  fn which_in_finds_executable_only() {
    let d = tmp("which");
    let exe = d.join("mytool");
    make_exec(&exe);
    let plain = d.join("notexe");
    std::fs::write(&plain, "data").unwrap();
    let path = OsString::from(d.to_str().unwrap());
    assert_eq!(which_in("mytool", &path), Some(exe));
    assert_eq!(which_in("notexe", &path), None);
    assert_eq!(which_in("missing", &path), None);
  }

  #[cfg(unix)]
  #[test]
  fn detect_prefers_docker_on_linux() {
    let d = tmp("detect-linux");
    make_exec(&d.join("docker"));
    make_exec(&d.join("podman"));
    let path = OsString::from(d.to_str().unwrap());
    assert_eq!(detect_runtime_in(false, &path).unwrap().name, "docker");
  }

  #[cfg(unix)]
  #[test]
  fn detect_falls_back_to_podman() {
    let d = tmp("detect-podman");
    make_exec(&d.join("podman"));
    let path = OsString::from(d.to_str().unwrap());
    assert_eq!(detect_runtime_in(false, &path).unwrap().name, "podman");
  }

  #[cfg(unix)]
  #[test]
  fn detect_prefers_apple_container_on_macos() {
    let d = tmp("detect-macos");
    make_exec(&d.join("container"));
    make_exec(&d.join("docker"));
    let path = OsString::from(d.to_str().unwrap());
    assert_eq!(detect_runtime_in(true, &path).unwrap().name, "container");
  }

  #[cfg(unix)]
  #[test]
  fn detect_none_when_empty() {
    let d = tmp("detect-empty");
    let path = OsString::from(d.to_str().unwrap());
    assert!(detect_runtime_in(false, &path).is_none());
  }

  #[test]
  fn snap_confined_paths_are_detected() {
    assert!(is_snap_confined(Path::new("/snap/bin/docker")));
    assert!(!is_snap_confined(Path::new("/usr/bin/docker")));
    assert!(!is_snap_confined(Path::new("/usr/local/bin/podman")));
  }

  #[cfg(unix)]
  #[test]
  fn detect_skips_snap_docker_for_another_runtime() {
    let d = tmp("detect-confined");
    std::fs::create_dir_all(d.join("snap/bin")).unwrap();
    std::fs::create_dir_all(d.join("bin")).unwrap();
    make_exec(&d.join("snap/bin/docker")); // snap-confined docker, first on PATH
    make_exec(&d.join("bin/podman"));
    let path = OsString::from(format!("{}:{}", d.join("snap/bin").display(), d.join("bin").display()));
    // docker is preferred by order but snap-confined → podman wins.
    assert_eq!(detect_runtime_in(false, &path).unwrap().name, "podman");
    // ...but a snap docker is still better than nothing when it's the only runtime.
    let only = OsString::from(d.join("snap/bin").to_str().unwrap());
    assert_eq!(detect_runtime_in(false, &only).unwrap().name, "docker");
  }

  #[test]
  fn image_tags_are_per_harness() {
    assert_eq!(image_tag(Harness::Opencode), "scsh-opencode:latest");
    assert_eq!(image_tag(Harness::Claude), "scsh-claude:latest");
  }

  #[test]
  fn dockerfile_has_shared_base_and_two_harness_targets() {
    let df = dockerfile();
    assert!(df.contains("FROM debian:bookworm-slim AS scsh-base"));
    assert!(df.contains("FROM scsh-base AS scsh-opencode"));
    assert!(df.contains("FROM scsh-base AS scsh-claude"));
    assert!(df.contains("npm install -g opencode-ai"));
    assert!(df.contains("npm install -g @anthropic-ai/claude-code"));
    assert!(!df.contains("CMD ["));
    assert!(df.contains("ENV SCSH_RUN_LOG=/home/agent/repo/tmp/scsh-run.log"));
    assert!(df.contains("ENV SCSH=1"));
  }

  #[test]
  fn dockerfile_opencode_stage_has_unattended_env() {
    let df = dockerfile();
    assert!(df.contains("ENV OPENCODE_YOLO=true"));
    assert!(df.contains("opencode --version"));
  }

  #[test]
  fn dockerfile_claude_stage_verifies_cli() {
    assert!(dockerfile().contains("claude --version"));
  }

  #[test]
  fn dockerfile_bakes_the_toolchain_and_excludes_java() {
    let df = dockerfile();
    for tool in [
      "python3",
      "python3-venv",
      "perl",
      "gawk",
      "build-essential",
      "pkg-config",
      "cmake",
      "jq",
      "sqlite3",
      "postgresql-client",
      "protobuf-compiler",
      "shellcheck",
      "git-lfs",
      "openssh-client",
      "iputils-ping",
      "traceroute",
      "netcat-openbsd",
      "astral-sh/uv",
      "mikefarah/yq",
      "dl.k8s.io",
      "cli.github.com",
      "go.dev/dl",
      "sh.rustup.rs",
      "awscli-exe-linux",
      "google-cloud-cli",
    ] {
      assert!(df.contains(tool), "image should install {tool}");
    }
    // Java is deliberately NOT installed (see the README).
    let lower = df.to_lowercase();
    assert!(!lower.contains("openjdk") && !lower.contains("-jdk") && !lower.contains("-jre"), "no Java by design");
    // UTF-8 locale + the builder host's timezone (a build arg).
    assert!(df.contains("ENV LANG=C.UTF-8"));
    assert!(df.contains("ARG TZ=UTC") && df.contains("ENV TZ=${TZ}"));
    // The Go/Rust toolchains the agent uses are on PATH.
    assert!(df.contains("/usr/local/go/bin") && df.contains("/usr/local/cargo/bin"));
  }

  #[test]
  fn dockerfile_is_platform_agnostic() {
    let df = dockerfile();
    // Every architecture-specific layer resolves the target arch at build time rather than
    // hardcoding one — uv/yq/kubectl/gh, Go, AWS, and gcloud each detect it.
    assert!(
      df.matches("dpkg --print-architecture").count() >= 4,
      "each arch-specific layer must resolve arch at build time"
    );
    // Both architecture families are mapped (Debian arch + the vendors' arch spellings).
    for token in ["amd64", "arm64", "x86_64", "aarch64"] {
      assert!(df.contains(token), "arch mapping must cover {token}");
    }
    // gcloud's arm tarball is spelled `-arm`, not `-arm64`.
    assert!(df.contains("google-cloud-cli-linux-${gclarch}"), "gcloud download must be arch-parameterized");
    // No download URL may pin a single architecture.
    for bad in [
      "uv-x86_64-unknown-linux-gnu",
      "yq_linux_amd64",
      "linux/amd64/kubectl",
      "linux-amd64.tar.gz",
      "awscli-exe-linux-x86_64.zip",
      "google-cloud-cli-linux-x86_64.tar.gz",
    ] {
      assert!(!df.contains(bad), "download URL must not hardcode an architecture: {bad}");
    }
  }

  #[test]
  fn dockerfile_matches_the_path_constants() {
    // The embedded Dockerfile is the source of truth, but it must stay consistent with the
    // Rust-side constants other code uses (the repo mount/WORKDIR, the XDG data dir scsh drops
    // the credential into, and the per-run log path).
    let df = dockerfile();
    assert!(df.contains(&format!("WORKDIR {AGENT_REPO}")), "WORKDIR must match AGENT_REPO");
    assert!(
      df.contains(&format!("ENV XDG_DATA_HOME={AGENT_REPO}/{AGENT_XDG_DATA_REL}")),
      "XDG_DATA_HOME must match AGENT_REPO/AGENT_XDG_DATA_REL"
    );
    assert!(
      df.contains(&format!("ENV {RUN_LOG_VAR}={AGENT_REPO}/{RUN_LOG_REL}")),
      "Dockerfile run-log ENV must match RUN_LOG_VAR and RUN_LOG_REL"
    );
  }

  #[test]
  fn dockerfile_keeps_home_separate_from_the_repo_mount() {
    // The repo is mounted at /home/agent/repo while $HOME stays /home/agent, so the harness's
    // home-dir scratch (caches/config) never lands in the cloned repo's working tree.
    let df = dockerfile();
    assert!(df.contains("ENV HOME=/home/agent"), "HOME must be the agent's home");
    assert!(df.contains(&format!("WORKDIR {AGENT_REPO}")));
    assert_ne!("/home/agent", AGENT_REPO, "the mount must not be the home dir");
    assert!(AGENT_REPO.starts_with("/home/agent/"), "the repo mount lives under the home dir");
    // The forwarded credential and the run log both live under the gitignored tmp/.
    assert!(AGENT_XDG_DATA_REL.starts_with("tmp/") && RUN_LOG_REL.starts_with("tmp/"));
  }

  #[test]
  fn dockerfile_creates_agent_user_and_runs_as_it() {
    let df = dockerfile();
    assert!(df.contains("ARG AGENT_UID=1000"));
    assert!(df.contains("ARG AGENT_GID=1000"));
    assert!(df.contains("WORKDIR /home/agent/repo"));
    assert!(df.contains("\nUSER agent\n"));
    // The agent user is created before the image switches to it.
    let agent_at = df.find("-d /home/agent").expect("agent-user layer");
    let user_at = df.find("\nUSER agent\n").expect("a USER layer");
    assert!(agent_at < user_at, "the agent user must exist before USER agent");
  }

  #[test]
  fn harness_command_builds_opencode_invocation() {
    assert_eq!(
      harness_command(Harness::Opencode, Some("openai/gpt-5.5"), "add", "tmp/add.json"),
      "opencode -m openai/gpt-5.5 run 'run skill add' 2>&1 | tee \"$SCSH_RUN_LOG\""
    );
    assert_eq!(
      harness_command(Harness::Opencode, None, "multiply", "tmp/mul.json"),
      "opencode run 'run skill multiply' 2>&1 | tee \"$SCSH_RUN_LOG\""
    );
  }

  #[test]
  fn harness_command_builds_claude_invocation() {
    let cmd = harness_command(Harness::Claude, Some("sonnet"), "add", "tmp/add_claude_sonnet_4_6_result.json");
    assert!(cmd.contains(".skills/add/SKILL.md"));
    assert!(cmd.contains("--model sonnet"));
    assert!(cmd.contains("tee \"$SCSH_RUN_LOG\""));
  }

  #[test]
  fn build_method_depends_on_runtime() {
    assert_eq!(build_method("container"), BuildMethod::ContextDir);
    assert_eq!(build_method("docker"), BuildMethod::Stdin);
    assert_eq!(build_method("podman"), BuildMethod::Stdin);
  }

  #[test]
  fn commands_have_expected_shape() {
    assert_eq!(
      build_command_stdin("docker", "scsh-opencode:latest", "scsh-opencode", 1006, 1007, "Europe/Berlin"),
      vec![
        "docker",
        "build",
        "-t",
        "scsh-opencode:latest",
        "--target",
        "scsh-opencode",
        "--build-arg",
        "AGENT_UID=1006",
        "--build-arg",
        "AGENT_GID=1007",
        "--build-arg",
        "TZ=Europe/Berlin",
        "-"
      ]
    );
    assert_eq!(
      run_command("docker", "scsh-opencode:latest", "/tmp/clone", "run-s", &[], &[], "opencode run 'run skill s'"),
      vec![
        "docker",
        "run",
        "--rm",
        "--name",
        "run-s",
        "-v",
        "/tmp/clone:/home/agent/repo",
        "scsh-opencode:latest",
        "/bin/sh",
        "-c",
        "opencode run 'run skill s'"
      ]
    );
    assert_eq!(
      run_command(
        "podman",
        "scsh-claude:latest",
        "/tmp/clone",
        "run-s",
        &[],
        &[("/home/u/.claude", "/home/agent/.claude:ro")],
        "claude -p hi"
      ),
      vec![
        "podman",
        "run",
        "--rm",
        "--name",
        "run-s",
        "--userns=keep-id",
        "-v",
        "/home/u/.claude:/home/agent/.claude:ro",
        "-v",
        "/tmp/clone:/home/agent/repo",
        "scsh-claude:latest",
        "/bin/sh",
        "-c",
        "claude -p hi"
      ]
    );
    assert_eq!(
      run_command(
        "docker",
        "scsh-opencode:latest",
        "/tmp/clone",
        "run-s",
        &[],
        &[(
          "/home/u/.local/share/opencode/auth.json",
          OPENCODE_AUTH_MOUNT
        )],
        "opencode run 'run skill s'"
      ),
      vec![
        "docker",
        "run",
        "--rm",
        "--name",
        "run-s",
        "-v",
        "/home/u/.local/share/opencode/auth.json:/home/agent/repo/tmp/.xdg-data/opencode/auth.json",
        "-v",
        "/tmp/clone:/home/agent/repo",
        "scsh-opencode:latest",
        "/bin/sh",
        "-c",
        "opencode run 'run skill s'"
      ]
    );
  }

  #[test]
  fn opencode_forward_mounts_maps_copied_tree() {
    let base = std::env::temp_dir().join(format!("scsh-opencode-forward-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("xdg/opencode")).unwrap();
    std::fs::create_dir_all(base.join("config/opencode")).unwrap();
    std::fs::write(base.join("xdg/opencode/auth.json"), "{}").unwrap();
    std::fs::write(base.join("config/opencode/opencode.json"), "{}").unwrap();
    let mounts = opencode_forward_mounts(&base);
    assert_eq!(mounts.len(), 2);
    assert_eq!(mounts[0].1, OPENCODE_AUTH_MOUNT);
    assert_eq!(mounts[1].1, OPENCODE_CONFIG_JSON_MOUNT);
    let _ = std::fs::remove_dir_all(&base);
  }

  #[test]
  fn opencode_host_mounts_empty_when_nothing_on_host() {
    assert!(opencode_host_mounts_from(None, None, None).is_empty());
  }

  #[test]
  fn run_command_forwards_env_as_e_flags() {
    let env = vec![("A".to_string(), "20".to_string()), ("B".to_string(), "22".to_string())];
    assert_eq!(
      run_command("docker", "scsh-opencode:latest", "/tmp/clone", "run-s", &env, &[], "opencode run 'run skill s'"),
      vec![
        "docker",
        "run",
        "--rm",
        "--name",
        "run-s",
        "-e",
        "A=20",
        "-e",
        "B=22",
        "-v",
        "/tmp/clone:/home/agent/repo",
        "scsh-opencode:latest",
        "/bin/sh",
        "-c",
        "opencode run 'run skill s'"
      ]
    );
  }

  #[test]
  fn clone_command_is_a_full_local_clone() {
    assert_eq!(clone_command("/repo", "/tmp/dst"), vec!["git", "clone", "/repo", "/tmp/dst"]);
  }

  #[test]
  fn claude_container_auth_accepts_oauth_token_env() {
    let key = CLAUDE_OAUTH_TOKEN_ENV;
    let prev = std::env::var_os(key);
    std::env::set_var(key, "test-token");
    assert!(claude_container_auth_ready());
    match prev {
      Some(v) => std::env::set_var(key, v),
      None => std::env::remove_var(key),
    }
  }

  #[test]
  fn check_claude_harness_errors_without_token_or_credentials_file() {
    let key = CLAUDE_OAUTH_TOKEN_ENV;
    let prev = std::env::var_os(key);
    std::env::remove_var(key);
    let err = check_harness_host(Harness::Claude).unwrap_err();
    assert!(err.contains("CLAUDE_CODE_OAUTH_TOKEN"));
    assert!(err.contains("setup-token"));
    match prev {
      Some(v) => std::env::set_var(key, v),
      None => std::env::remove_var(key),
    }
  }

  #[test]
  fn utc_timestamp_formats_known_epochs() {
    assert_eq!(format_utc_timestamp(0), "19700101-000000");
    assert_eq!(format_utc_timestamp(1_700_000_000), "20231114-221320");
  }

  #[test]
  fn run_dir_and_backup_names() {
    assert_eq!(run_dir_name(1_700_000_000, "add", "docker"), "scsh-20231114-221320-utc-run-add");
    // skill names are sanitized for the filesystem.
    assert_eq!(run_dir_name(0, "My Skill!", "docker"), "scsh-19700101-000000-utc-run-my-skill");
    assert_eq!(backup_name("add_result.json", 1_700_000_000), "add_result.json.bak.20231114-221320-utc");
  }

  #[test]
  fn truncate_middle_keeps_ends() {
    assert_eq!(truncate_middle("abcdef", 6), "abcdef");
    assert_eq!(truncate_middle("abcdefgh", 6), "ab..gh");
    assert_eq!(truncate_middle("abcdefgh", 5), "ab..h");
  }

  #[test]
  fn apple_container_run_dir_fits_long_reviewer_names() {
    let skill = "reviewability-reviewer-opencode-glm-5.2";
    let name = apple_container_run_dir_name_with_nonce(skill, "abcdef");
    assert_eq!(name, "scsh-abcdef-run-reviewability-reviewer-opencode-glm-5.2");
    assert!(name.len() <= CONTAINER_ID_MAX_LEN);
    assert!(is_scsh_run_dir_name(&name));
  }

  #[test]
  fn apple_container_run_dir_middle_truncates_when_needed() {
    let skill = "a".repeat(80);
    let name = apple_container_run_dir_name_with_nonce(&skill, "abcdef");
    assert!(name.len() <= CONTAINER_ID_MAX_LEN);
    assert!(name.contains(".."));
    assert!(name.starts_with("scsh-abcdef-run-"));
    assert!(is_scsh_run_dir_name(&name));
  }

  #[test]
  fn is_scsh_run_dir_name_recognizes_both_formats() {
    assert!(is_scsh_run_dir_name("scsh-20231114-221320-utc-run-add"));
    assert!(is_scsh_run_dir_name("scsh-abcdef-run-add"));
    assert!(!is_scsh_run_dir_name("scsh-installskills-1-2"));
    assert!(!is_scsh_run_dir_name("scsh-abcdefg-run-add"));
  }

  #[test]
  fn branch_materialization_skips_head_and_current() {
    let refs = "origin/HEAD\norigin/main\norigin/feature-x\norigin/release\n";
    assert_eq!(local_branches_to_create(refs, "main"), vec!["feature-x", "release"]);
    // nothing to create when only HEAD and the current branch exist.
    assert!(local_branches_to_create("origin/HEAD\norigin/main\n", "main").is_empty());
  }

  #[test]
  fn shell_join_quotes_when_needed() {
    assert_eq!(shell_join(&["docker".into(), "build".into()]), "docker build");
    assert_eq!(shell_join(&["a b".into()]), "'a b'");
  }
}
