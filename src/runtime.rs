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

/// Candidate runtimes scsh auto-detects, in order. **On macOS, Apple's `container` is the
/// only auto-detected runtime — scsh never silently falls back to Docker or Podman there.**
/// A macOS user who wants Docker/Podman must ask for it explicitly with `SCSH_RUNTIME=docker`
/// (which [`detect_runtime`] honors regardless of this list). Off macOS, Docker is primary and
/// Podman the fallback.
pub fn runtime_candidates(is_macos: bool) -> &'static [&'static str] {
  if is_macos {
    &["container"]
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

/// Tag for the shared base image (`scsh-base` Dockerfile stage).
pub const BASE_IMAGE_TAG: &str = "scsh-base:latest";

/// Dockerfile `--target` for the shared base image.
pub const BASE_IMAGE_TARGET: &str = "scsh-base";

/// Fingerprint for [`BASE_IMAGE_TAG`] (toolchain layer only; harness stages excluded).
pub fn base_image_fingerprint(dockerfile: &str, uid: u32, gid: u32, tz: &str) -> String {
  image_build_fingerprint(dockerfile, BASE_IMAGE_TARGET, uid, gid, tz)
}

/// The tag of the harness-specific image scsh builds.
pub fn image_tag(harness: Harness) -> String {
  match harness {
    Harness::Opencode => "scsh-opencode:latest".to_string(),
    Harness::Claude => "scsh-claude:latest".to_string(),
    Harness::Codex => "scsh-codex:latest".to_string(),
    Harness::Grok => "scsh-grok:latest".to_string(),
    Harness::Cursor => "scsh-cursor:latest".to_string(),
  }
}

/// The Dockerfile build `--target` for a harness image.
pub fn image_target(harness: Harness) -> &'static str {
  match harness {
    Harness::Opencode => "scsh-opencode",
    Harness::Claude => "scsh-claude",
    Harness::Codex => "scsh-codex",
    Harness::Grok => "scsh-grok",
    Harness::Cursor => "scsh-cursor",
  }
}

/// Run-dir-relative path where scsh copies forwarded Claude auth before a run (gitignored
/// `tmp/`). The image sets `CLAUDE_CONFIG_DIR` to this tree's `.claude` dir, so the config
/// rides along with the repo mount and stays writable — no bind mounts, same pattern as
/// codex/grok/cursor. (Single-file bind mounts are read-only under Apple containers, and
/// Claude Code's interactive TUI re-runs onboarding when it cannot write its state json.)
pub const CLAUDE_AUTH_REL: &str = "tmp/.claude-auth";

/// Run-dir-relative opencode auth dir (`$XDG_DATA_HOME/opencode` in the image). scsh copies the
/// host's `~/.local/share/opencode/auth.json` into the run clone here (riding the repo mount),
/// required for third-party opencode providers (e.g. Nebius GLM) that authenticate via the host
/// login rather than a built-in model route.
pub const OPENCODE_DATA_REL: &str = "tmp/.xdg-data/opencode";

/// Run-dir-relative opencode config dir (`$XDG_CONFIG_HOME/opencode` in the image). Custom
/// providers (e.g. Nebius GLM) are declared here; auth.json alone is not enough. scsh copies the
/// host config into each run clone here so it rides the repo mount (no separate bind mounts).
pub const OPENCODE_CONFIG_REL: &str = "tmp/.config/opencode";

/// Host env var for long-lived Claude OAuth (`claude setup-token`).
pub const CLAUDE_OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Run-dir-relative Codex home. The image sets `CODEX_HOME` to [`AGENT_REPO`]`/`this, so
/// forwarding host credentials is just copying `auth.json`/`config.toml` here — the tree is
/// under the gitignored `tmp/`, which is visible in-container in BOTH repo mount modes.
/// Codex's own per-run session/log data lands here too (readable on the host afterwards).
pub const CODEX_FORWARD_REL: &str = "tmp/.codex";

/// Host env var for API-key Codex auth (works headless; ChatGPT-plan auth uses auth.json).
pub const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// The host's Codex home: `$CODEX_HOME` or `~/.codex`.
pub fn codex_home_on_host() -> Option<PathBuf> {
  if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|d| !d.is_empty()) {
    return Some(PathBuf::from(dir));
  }
  std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex"))
}

/// Host `auth.json` written by `codex login` (ChatGPT plan or API-key login).
pub fn codex_auth_file_on_host() -> Option<PathBuf> {
  codex_home_on_host().map(|d| d.join("auth.json")).filter(|p| p.is_file())
}

/// Whether the host has credentials codex containers can use: `auth.json` or `OPENAI_API_KEY`.
pub fn codex_container_auth_ready() -> bool {
  codex_auth_file_on_host().is_some() || std::env::var(OPENAI_API_KEY_ENV).map(|v| !v.is_empty()).unwrap_or(false)
}

/// Run-dir-relative Grok home (same pattern as codex): the image sets `GROK_HOME` to
/// [`AGENT_REPO`]`/`this, so forwarding host credentials is just copying files here.
pub const GROK_FORWARD_REL: &str = "tmp/.grok";

/// Host env var for API-key Grok auth (xAI API key from console.x.ai; works headless).
pub const XAI_API_KEY_ENV: &str = "XAI_API_KEY";

/// The host's Grok home: `$GROK_HOME` or `~/.grok`.
pub fn grok_home_on_host() -> Option<PathBuf> {
  if let Some(dir) = std::env::var_os("GROK_HOME").filter(|d| !d.is_empty()) {
    return Some(PathBuf::from(dir));
  }
  std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".grok"))
}

/// Host `auth.json` written by `grok login` (browser OIDC or device code).
pub fn grok_auth_file_on_host() -> Option<PathBuf> {
  grok_home_on_host().map(|d| d.join("auth.json")).filter(|p| p.is_file())
}

/// Whether the host has credentials grok containers can use: `auth.json` or `XAI_API_KEY`.
pub fn grok_container_auth_ready() -> bool {
  grok_auth_file_on_host().is_some() || std::env::var(XAI_API_KEY_ENV).map(|v| !v.is_empty()).unwrap_or(false)
}

/// Whether grok's stored OAuth login has lapsed: `~/.grok/auth.json` carries an `expires_at`
/// ISO-8601 timestamp, and once its date is before today the interactive Build TUI stops
/// trusting the session and demands a fresh browser sign-in (headless `grok -p` silently
/// refreshes, so it doesn't hit this). When true, the run skips grok with a "sign in on the
/// host" message rather than hanging on the un-clickable browser-auth screen in the container.
/// Best-effort: an unreadable/parse-less file returns `false` (don't block on uncertainty).
pub fn grok_auth_expired() -> bool {
  let Some(path) = grok_auth_file_on_host() else { return false };
  let Ok(text) = std::fs::read_to_string(&path) else { return false };
  // Pull the value after the first `"expires_at"` key — a quoted ISO date like
  // "2026-07-03T22:28:34.057655Z". Compare its date (YYYYMMDD) to today's, lexicographically.
  let Some(rest) = text.split("\"expires_at\"").nth(1).and_then(|s| s.split(':').nth(1)) else { return false };
  let iso: String = rest.trim().trim_start_matches('"').chars().take(10).collect(); // "2026-07-03"
  let expires: String = iso.chars().filter(|c| c.is_ascii_digit()).collect(); // "20260703"
  if expires.len() != 8 {
    return false;
  }
  let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
  let today = &format_utc_timestamp(now)[..8]; // "20260707"
  expires.as_str() < today
}

/// Run-dir-relative Cursor config dir (`CURSOR_CONFIG_DIR` inside the container).
pub const CURSOR_FORWARD_REL: &str = "tmp/.cursor";

/// Run-dir-relative Linux auth dir (`$XDG_CONFIG_HOME/cursor/auth.json` in the container).
pub const CURSOR_AUTH_FORWARD_REL: &str = "tmp/.config/cursor";

/// Host env var for API-key Cursor auth (Cursor Dashboard → API Keys; works headless).
pub const CURSOR_API_KEY_ENV: &str = "CURSOR_API_KEY";

/// In-container env var for Cursor CLI config (cli-config.json, mcp.json).
pub const CURSOR_CONFIG_DIR_ENV: &str = "CURSOR_CONFIG_DIR";

/// In-container env var so Linux cursor-agent finds auth.json under tmp/.config/cursor/.
pub const XDG_CONFIG_HOME_ENV: &str = "XDG_CONFIG_HOME";

/// The host's Cursor config dir: `$CURSOR_HOME` or `~/.cursor`.
pub fn cursor_home_on_host() -> Option<PathBuf> {
  if let Some(dir) = std::env::var_os("CURSOR_HOME").filter(|d| !d.is_empty()) {
    return Some(PathBuf::from(dir));
  }
  std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cursor"))
}

/// Host `auth.json` when cursor-agent wrote tokens to disk (typical on Linux).
pub fn cursor_auth_file_on_host() -> Option<PathBuf> {
  let home = std::env::var_os("HOME").map(PathBuf::from)?;
  for path in [home.join(".config/cursor/auth.json"), home.join(".cursor/auth.json")] {
    if path.is_file() {
      return Some(path);
    }
  }
  None
}

/// Non-empty `CURSOR_API_KEY` on the host, if set.
pub fn cursor_api_key() -> Option<String> {
  std::env::var(CURSOR_API_KEY_ENV).ok().filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn keychain_secret(service: &str) -> Option<String> {
  let out =
    std::process::Command::new("security").args(["find-generic-password", "-s", service, "-w"]).output().ok()?;
  if !out.status.success() {
    return None;
  }
  let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
  (!token.is_empty()).then_some(token)
}

#[cfg(not(target_os = "macos"))]
fn keychain_secret(_service: &str) -> Option<String> {
  None
}

/// macOS stores cursor-agent OAuth tokens in the login keychain after `cursor agent login`.
pub fn cursor_keychain_access_token() -> Option<String> {
  keychain_secret("cursor-access-token")
}

pub fn cursor_keychain_refresh_token() -> Option<String> {
  keychain_secret("cursor-refresh-token")
}

/// macOS stores Claude Code's OAuth credentials in the login keychain — the item is the
/// literal JSON of `.credentials.json` (accessToken + refreshToken + expiresAt + scopes).
/// The interactive TUI treats a credentials file without expiry/scopes as logged-out, so
/// forwarding this full blob (not just an access token) is what makes the TUI skip login.
pub fn claude_keychain_credentials_json() -> Option<String> {
  keychain_secret("Claude Code-credentials").filter(|s| s.contains("claudeAiOauth"))
}

/// Whether the host has credentials cursor containers can use.
pub fn cursor_container_auth_ready() -> bool {
  cursor_api_key().is_some() || cursor_auth_file_on_host().is_some() || cursor_keychain_access_token().is_some()
}

/// Absolute path the repo clone is bind-mounted at, and the image's WORKDIR (where the harness
/// starts). Deliberately a *subdirectory* of the agent user's home (`/home/agent`), not the
/// home itself: the harness and its tools scribble into `$HOME` (`~/.cache`, `~/.config`,
/// `~/.npm`, …), so keeping the clone one level down keeps that scratch out of the repo's
/// working tree. The home is set in the image (see `src/Dockerfile`).
pub const AGENT_REPO: &str = "/home/agent/repo";

/// opencode's data dir (`XDG_DATA_HOME`), RELATIVE to the repo, where scsh drops the forwarded
/// Per-run log path the harness tees every line of its output to, RELATIVE to the repo. It
/// lives under the gitignored `tmp/` (so it is never an untracked file); on the host it is
/// therefore `<run_dir>/tmp/scsh-run.log`, where the full intra-container output can be read.
pub const RUN_LOG_REL: &str = "tmp/scsh-run.log";

/// Per-run asciinema recording (asciicast v3, NDJSON) of the harness PTY, RELATIVE to the
/// repo: `${SCSH_RUN_LOG}.cast` in-container, `<run_dir>/tmp/scsh-run.log.cast` on the host.
/// NDJSON means any byte-prefix ending on a newline is itself a valid (partial) recording,
/// so the file can be downloaded and replayed while the skill is still running.
pub const RUN_CAST_REL: &str = "tmp/scsh-run.log.cast";

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

/// Whether harness commands run at full debug verbosity. On by default — the live board,
/// `tmp/scsh-run.log`, and the session browser want turn-by-turn output. Opt out with
/// `SCSH_QUIET=1`: harnesses then log at their default level (output is still teed to the log).
pub fn harness_verbose_enabled() -> bool {
  !matches!(std::env::var("SCSH_QUIET").ok().as_deref(), Some("1") | Some("true"))
}

/// Extra `-e` pairs injected into every skill container so harnesses log generously.
pub fn harness_container_env(harness: Harness) -> Vec<(String, String)> {
  harness_container_env_verbose(harness, harness_verbose_enabled())
}

fn harness_container_env_verbose(harness: Harness, verbose: bool) -> Vec<(String, String)> {
  match harness {
    Harness::Opencode => vec![("OPENCODE_CLIENT".to_string(), "scsh".to_string())],
    Harness::Claude if verbose => {
      vec![("DEBUG".to_string(), "1".to_string()), ("CLAUDE_CODE_DEBUG_LOG_LEVEL".to_string(), "verbose".to_string())]
    }
    Harness::Claude => Vec::new(),
    // Codex is a Rust CLI; RUST_LOG enables its tracing output (stderr → the teed run log).
    Harness::Codex if verbose => vec![("RUST_LOG".to_string(), "codex_core=info,codex_exec=info".to_string())],
    Harness::Codex => Vec::new(),
    // Grok's verbosity comes from --debug/--debug-file flags; no env needed.
    Harness::Grok => Vec::new(),
    // Point cursor-agent at forwarded config + auth under the repo's gitignored tmp/.
    Harness::Cursor => vec![
      (CURSOR_CONFIG_DIR_ENV.to_string(), format!("{AGENT_REPO}/{CURSOR_FORWARD_REL}")),
      (XDG_CONFIG_HOME_ENV.to_string(), format!("{AGENT_REPO}/tmp/.config")),
    ],
  }
}

/// The shell command a harness runs *inside the container* for one skill.
/// Output is always teed to [`RUN_LOG_VAR`] for the daemon; `SCSH_QUIET=1` drops the debug flags.
/// `effort` is the `.scsh.yml` reasoning-effort level (codex and grok only; expansion
/// guarantees it is `None` for harnesses without an effort knob).
/// The prompt's opening sentence: where the agent finds the skill. Repo-delivered skills sit
/// at the committed path; a global install is invoked by NAME where the CLI discovers
/// user-level skills (claude, cursor), and by its container path otherwise.
pub fn skill_prompt_clause(harness: Harness, skill_source: &str, global: bool) -> String {
  if !global {
    return format!("Run the skill defined in .skills/{skill_source}/SKILL.md.");
  }
  if harness.resolves_skills_by_name() {
    format!(
      "Use your globally installed skill named '{skill_source}' (it resolves as /{skill_source};        its SKILL.md is in your user-level skills directory, not in this repository)."
    )
  } else {
    format!("Run the skill defined in {}/{skill_source}/SKILL.md.", harness.global_skills_rel())
  }
}

pub fn harness_command(
  harness: Harness, model: Option<&str>, effort: Option<&str>, skill_source: &str, result: &str,
  term: crate::config::Terminal, global_skill: bool,
) -> String {
  let skill_clause = skill_prompt_clause(harness, skill_source, global_skill);
  match harness {
    Harness::Opencode => {
      let prompt = format!(
        "{skill_clause} Follow its instructions exactly. \
         Write the required result file to the path in the SCSH_RESULT environment variable. \
         Do not git fetch, pull, push, or clone — scsh preloaded a full local clone; use only refs already present."
      );
      // Full interactive TUI: opencode's default command IS the TUI, and `--prompt` seeds the
      // initial message (vs the old headless `opencode run`, which produced no recording — and
      // here no result). The ephemeral container is the sandbox.
      let mut tui = String::from("opencode");
      if let Some(m) = model {
        tui.push_str(" -m ");
        tui.push_str(&shell_quote(m));
      }
      tui.push_str(" --prompt ");
      tui.push_str(&shell_quote(&prompt));
      // opencode --prompt only PRE-FILLS the input box; send Enter once the TUI is up to submit.
      wrap_tui_shell(harness, skill_source, model, &tui, TuiQuit::DoubleCtrlC, TuiSubmit::Enter, result, term)
    }
    Harness::Claude => {
      let prompt = format!(
        "{skill_clause} Follow its instructions exactly. \
         Write the required result file to the path in the SCSH_RESULT environment variable. \
         Do not git fetch, pull, push, or clone — scsh preloaded a full local clone; use only refs already present."
      );
      // Full interactive TUI (no -p): the recording shows the real Claude Code screen, and
      // no dialog blocks it. `bypassPermissions` auto-approves EVERY tool (bash, edits, fetch,
      // MCP, …) — scsh runs arbitrary skills, so a per-tool allowlist would not be enough. Its
      // consent screen is suppressed by forwarding a MINIMAL `.claude.json` (see main's
      // forward_claude_auth): the full ~49 KB host config re-triggered the consent, a tiny one
      // (login identity + onboarding/trust/bypass-accepted) does not. All config, no scraping.
      let mut tui = String::from("claude --permission-mode bypassPermissions");
      if let Some(m) = model {
        tui.push_str(" --model ");
        tui.push_str(&shell_quote(m));
      }
      tui.push(' ');
      tui.push_str(&shell_quote(&prompt));
      wrap_tui_shell(harness, skill_source, model, &tui, TuiQuit::SlashExit, TuiSubmit::Auto, result, term)
    }
    Harness::Codex => {
      let prompt = format!(
        "{skill_clause} Follow its instructions exactly. \
         Write the required result file to the path in the SCSH_RESULT environment variable. \
         Do not git fetch, pull, push, or clone — scsh preloaded a full local clone; use only refs already present."
      );
      // Full interactive TUI (no `exec`): the recording shows the real Codex screen. The
      // container IS the sandbox (ephemeral, --rm), so codex's own sandbox/approvals are
      // bypassed; the repo mount is pre-trusted in the forwarded config.toml (see main's
      // forward_codex), so no dialog blocks.
      let mut tui = String::from("codex --dangerously-bypass-approvals-and-sandbox");
      if let Some(m) = model {
        tui.push_str(" -m ");
        tui.push_str(&shell_quote(m));
      }
      if let Some(e) = effort {
        tui.push_str(" -c ");
        tui.push_str(&shell_quote(&format!("model_reasoning_effort={e}")));
      }
      tui.push(' ');
      tui.push_str(&shell_quote(&prompt));
      wrap_tui_shell(harness, skill_source, model, &tui, TuiQuit::DoubleCtrlC, TuiSubmit::Auto, result, term)
    }
    Harness::Grok => {
      let prompt = format!(
        "{skill_clause} Follow its instructions exactly. \
         Write the required result file to the path in the SCSH_RESULT environment variable. \
         Do not git fetch, pull, push, or clone — scsh preloaded a full local clone; use only refs already present."
      );
      // Full interactive TUI: grok's default IS the Build TUI, and a positional prompt seeds
      // the interactive session (`grok "fix the bug"`) — vs the old headless `grok -p`, which
      // recorded no real terminal. `--always-approve` auto-approves; the container is the sandbox.
      let mut tui = String::from("grok --always-approve");
      if let Some(m) = model {
        tui.push_str(" -m ");
        tui.push_str(&shell_quote(m));
      }
      if let Some(e) = effort {
        tui.push_str(" --effort ");
        tui.push_str(&shell_quote(e));
      }
      tui.push(' ');
      tui.push_str(&shell_quote(&prompt)); // positional initial prompt
      wrap_tui_shell(harness, skill_source, model, &tui, TuiQuit::DoubleCtrlC, TuiSubmit::Auto, result, term)
    }
    Harness::Cursor => {
      let prompt = format!(
        "{skill_clause} Follow its instructions exactly. \
         Write the required result file to the path in the SCSH_RESULT environment variable. \
         Do not git fetch, pull, push, or clone — scsh preloaded a full local clone; use only refs already present."
      );
      // Full interactive TUI (no -p): the recording shows the real cursor-agent screen.
      // The ephemeral container is the sandbox; --force auto-approves. cursor's `--trust`
      // is print-mode-only, and its TUI workspace-trust prompt has no flag or seedable
      // config key — cursor records trust as a marker file under $HOME (NOT the forwarded
      // config dir), so it is created in-container just before the TUI starts. The repo
      // path slug is `/`-stripped, `/`->`-` of AGENT_REPO.
      let trust_dir = format!("$HOME/.cursor/projects/{}", AGENT_REPO.trim_start_matches('/').replace('/', "-"));
      // No `exec`: the wrapping shell must survive cursor-agent to record its exit status.
      let mut tui = format!(
        "mkdir -p {trust_dir} && : > {trust_dir}/.workspace-trusted && cursor-agent --force --sandbox disabled"
      );
      if let Some(m) = model {
        tui.push_str(" --model ");
        tui.push_str(&shell_quote(&cursor_model_with_effort(m, effort)));
      }
      tui.push(' ');
      tui.push_str(&shell_quote(&prompt));
      wrap_tui_shell(harness, skill_source, model, &tui, TuiQuit::DoubleCtrlC, TuiSubmit::Auto, result, term)
    }
  }
}

/// How to politely close a harness TUI once the skill's result file exists. The value is
/// passed verbatim to `scsh-tui-record`, which maps it to the harness's quit keystrokes.
#[derive(Debug, Clone, Copy)]
enum TuiQuit {
  /// Type `/exit` + Enter (Claude Code).
  SlashExit,
  /// Ctrl-C twice, one second apart (codex, cursor-agent quit-confirm flows).
  DoubleCtrlC,
}

impl TuiQuit {
  /// The `scsh-tui-record` argument selecting this quit style.
  fn as_arg(self) -> &'static str {
    match self {
      TuiQuit::SlashExit => "slash-exit",
      TuiQuit::DoubleCtrlC => "double-ctrl-c",
    }
  }
}

/// Whether a harness's initial prompt needs an explicit Enter to submit once the TUI is up.
/// claude/codex/cursor/grok auto-run their prompt; opencode's `--prompt` only pre-fills the
/// input box, so it must be submitted.
#[derive(Debug, Clone, Copy)]
enum TuiSubmit {
  /// The prompt auto-runs; send nothing.
  Auto,
  /// Send Enter once the TUI is up (opencode).
  Enter,
}

impl TuiSubmit {
  fn as_arg(self) -> &'static str {
    match self {
      TuiSubmit::Auto => "none",
      TuiSubmit::Enter => "enter",
    }
  }
}

/// Build the `scsh-tui-record` invocation that records a harness's interactive TUI.
///
/// The heavy lifting lives in the `scsh-tui-record` script baked into the base image (see
/// `src/Dockerfile`), so this stays a clean argv, not an inline shell program. The script
/// runs the harness TUI inside a `term.cols` x `term.rows` tmux session, records the
/// attached screen with asciinema to `${SCSH_RUN_LOG}.cast`, and — when the skill's
/// `result` file appears (the run's completion signal) — sends the harness its quit keys
/// and ends the recording. There is deliberately NO screen-scraping: every harness is
/// configured (flags + seeded config) so no consent/trust/login dialog ever appears; a
/// harness that still blocks is a setup bug that should surface, not be auto-clicked.
///
/// The output still tees to the run log, and scsh's container timeout remains the hard stop.
fn wrap_tui_shell(
  harness: Harness, skill_source: &str, model: Option<&str>, tui_cmd: &str, quit: TuiQuit, submit: TuiSubmit,
  result: &str, term: crate::config::Terminal,
) -> String {
  let model_label = model.unwrap_or("(harness default)");
  // `scsh-tui-record` records the harness's exit status to `${SCSH_RUN_LOG}.exit` via an EXIT
  // trap it wraps around this command, and a per-signal trace to `${SCSH_RUN_LOG}.tuidebug`.
  // scsh otherwise never sees the exit (asciinema and the tmux pane both swallow it), which makes
  // a harness that dies abnormally (crash, signal, OOM → 137/143/130) indistinguishable from one
  // that merely wrote no result. A trap is used rather than a bare `; echo $?` so a catchable
  // signal still records — an ABSENT .exit then uniquely means an uncatchable SIGKILL.
  format!(
    "{{ mkdir -p \"$(dirname \"${{{log_var}}}\")\"; \
echo \"scsh: harness={} skill={skill_source} model={model_label} tui=tmux \
log=${{{log_var}}} cast=${{{log_var}}}.cast\" >&2; \
scsh-tui-record {cols} {rows} {quit} {submit} {result_q} {tui_q}; }} 2>&1 | tee \"${{{log_var}}}\"",
    harness.as_str(),
    log_var = RUN_LOG_VAR,
    cols = term.cols,
    rows = term.rows,
    quit = quit.as_arg(),
    submit = submit.as_arg(),
    result_q = shell_quote(result),
    tui_q = shell_quote(tui_cmd),
  )
}

/// Cursor `--model` slugs use hyphen suffixes (`claude-opus-4-8-low`, `gpt-5.5-high`), not
/// bracket overrides. composer-2.5 only exposes `composer-2.5` and `composer-2.5-fast`.
fn cursor_model_with_effort(model: &str, effort: Option<&str>) -> String {
  if model.contains('[') {
    return model.to_string();
  }
  let Some(effort) = effort else {
    return model.to_string();
  };
  if model == "composer-2.5" || model.starts_with("composer-2.5-") {
    return match effort {
      "high" => "composer-2.5-fast".to_string(),
      _ => "composer-2.5".to_string(),
    };
  }
  let suffix = match effort {
    "xhigh" if model.starts_with("gpt-5.5") => "extra-high",
    other => other,
  };
  if model.ends_with(&format!("-{suffix}")) {
    return model.to_string();
  }
  format!("{model}-{suffix}")
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

/// Apple Containers sends the Dockerfile in a gRPC *header* (default 16 KiB). At or
/// above this size, `container build` fails with `Stream unexpectedly closed` /
/// `Transport became inactive` ([apple/container#735](https://github.com/apple/container/issues/735)).
/// scsh keeps `src/Dockerfile` under [`APPLE_CONTAINER_DOCKERFILE_SOFT_LIMIT`] and
/// comment-strips before every Apple build so macOS / Apple Silicon stays the default path.
pub const APPLE_CONTAINER_DOCKERFILE_HARD_LIMIT: usize = 16 * 1024;

/// Soft ceiling for the embedded Dockerfile (unit-tested). Leaves headroom under the
/// hard gRPC limit after labels / build-arg metadata Apple may also stuff into headers.
pub const APPLE_CONTAINER_DOCKERFILE_SOFT_LIMIT: usize = 15_000;

const _: () = assert!(APPLE_CONTAINER_DOCKERFILE_SOFT_LIMIT < APPLE_CONTAINER_DOCKERFILE_HARD_LIMIT);

/// Pick the build method for a runtime.
pub fn build_method(runtime: &str) -> BuildMethod {
  if runtime == "container" {
    BuildMethod::ContextDir
  } else {
    BuildMethod::Stdin
  }
}

/// Dockerfile text that will actually be built (and fingerprinted) for `runtime`.
/// On Apple Containers this is a comment-stripped copy so we stay under the gRPC
/// header limit; docker/podman get the embedded file verbatim.
pub fn dockerfile_for_runtime(runtime: &str) -> String {
  let raw = dockerfile();
  if runtime == "container" {
    compact_dockerfile_for_apple(&raw)
  } else {
    raw
  }
}

/// Strip full-line comments (and comment-only lines inside `<<…` heredocs) so the
/// Dockerfile fits Apple Containers' gRPC header limit. Instructions and heredoc
/// *code* are preserved; `# syntax=` is kept.
pub fn compact_dockerfile_for_apple(src: &str) -> String {
  let mut out = String::with_capacity(src.len());
  let mut heredoc_tag: Option<String> = None;
  let mut blank_pending = false;
  for line in src.lines() {
    if let Some(tag) = heredoc_tag.as_deref() {
      if line.trim() == tag {
        heredoc_tag = None;
        out.push_str(line);
        out.push('\n');
        blank_pending = false;
        continue;
      }
      let trimmed = line.trim_start();
      if trimmed.starts_with('#') && !trimmed.starts_with("#!") {
        continue;
      }
      out.push_str(line);
      out.push('\n');
      blank_pending = false;
      continue;
    }

    if let Some(tag) = dockerfile_heredoc_tag(line) {
      heredoc_tag = Some(tag.to_string());
      out.push_str(line);
      out.push('\n');
      blank_pending = false;
      continue;
    }

    let trimmed = line.trim();
    if trimmed.is_empty() {
      blank_pending = true;
      continue;
    }
    if trimmed.starts_with('#') && !trimmed.starts_with("# syntax=") {
      continue;
    }
    if blank_pending && !out.is_empty() {
      out.push('\n');
    }
    blank_pending = false;
    out.push_str(line);
    out.push('\n');
  }
  out
}

/// `<<'TAG'`, `<<"TAG"`, or `<<TAG` on a Dockerfile line → the terminator tag.
fn dockerfile_heredoc_tag(line: &str) -> Option<&str> {
  let idx = line.find("<<")?;
  let rest = line[idx + 2..].trim_start();
  if let Some(r) = rest.strip_prefix('\'') {
    return r.split('\'').next().filter(|s| !s.is_empty());
  }
  if let Some(r) = rest.strip_prefix('"') {
    return r.split('"').next().filter(|s| !s.is_empty());
  }
  rest.split_whitespace().next().filter(|s| !s.is_empty())
}

/// True when `dockerfile` cannot be sent to Apple's builder (hard gRPC limit).
pub fn apple_dockerfile_too_large(dockerfile: &str) -> bool {
  dockerfile.len() >= APPLE_CONTAINER_DOCKERFILE_HARD_LIMIT
}

/// User-facing error when a compacted Dockerfile still exceeds Apple's limit.
pub fn apple_dockerfile_too_large_message(bytes: usize) -> String {
  format!(
    "Dockerfile is {bytes} bytes after compacting for Apple Containers; \
     `container build` rejects files ≥ {APPLE_CONTAINER_DOCKERFILE_HARD_LIMIT} bytes \
     (gRPC header limit — https://github.com/apple/container/issues/735). \
     Trim comments/stages in src/Dockerfile, or opt into Docker with SCSH_RUNTIME=docker."
  )
}

/// Rewrite Apple's opaque build failures into an actionable message when the excerpt
/// matches the known gRPC-header / wedged-BuildKit symptoms.
pub fn rewrite_apple_build_failure(excerpt: &str) -> Option<String> {
  let lower = excerpt.to_lowercase();
  let grpc = lower.contains("stream unexpectedly closed") || lower.contains("transport became inactive");
  if !grpc {
    return None;
  }
  Some(format!(
    "{excerpt}\n\
     hint: Apple Containers often fails this way when the Dockerfile is near/over 16KB \
     (https://github.com/apple/container/issues/735), or when BuildKit is wedged. \
     scsh already comment-strips the Dockerfile; if it still fails, reset the builder:\n\
       container builder delete --force && container builder start --cpus 6 --memory 8G\n\
     Or opt into Docker with SCSH_RUNTIME=docker."
  ))
}

/// OCI label scsh stamps on every harness image at build time. Compared on later runs
/// to skip rebuilding when the embedded Dockerfile and build args are unchanged.
pub const BUILD_FINGERPRINT_LABEL: &str = "scsh.build.fingerprint";

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

fn build_labels(fingerprint: &str) -> Vec<String> {
  vec!["--label".into(), format!("{BUILD_FINGERPRINT_LABEL}={fingerprint}")]
}

/// Deterministic sha256 over the Dockerfile, `--target`, and the build args that affect the image.
pub fn image_build_fingerprint(dockerfile: &str, target: &str, uid: u32, gid: u32, tz: &str) -> String {
  let blob = format!("target={target}\nuid={uid}\ngid={gid}\ntz={tz}\n---\n{dockerfile}");
  crate::sha256::sha256_hex(blob.as_bytes())
}

/// Read the fingerprint label from an existing harness image, if present.
pub fn image_inspect_fingerprint(runtime: &str, tag: &str) -> Option<String> {
  use std::process::Command;
  let out = if runtime == "container" {
    Command::new("container").args(["image", "inspect", tag]).output().ok()?
  } else {
    let format = format!(r#"{{{{index .Config.Labels "{BUILD_FINGERPRINT_LABEL}"}}}}"#);
    Command::new(runtime).args(["image", "inspect", tag, "--format", &format]).output().ok()?
  };
  if !out.status.success() {
    return None;
  }
  let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
  if runtime == "container" {
    parse_label_from_container_inspect(&s, BUILD_FINGERPRINT_LABEL)
  } else if s.is_empty() {
    None
  } else {
    Some(s)
  }
}

/// True when `tag` exists and carries the expected build fingerprint (skip rebuild).
pub fn image_is_up_to_date(runtime: &str, tag: &str, fingerprint: &str) -> bool {
  image_inspect_fingerprint(runtime, tag).as_deref() == Some(fingerprint)
}

/// Pull a string label out of Apple `container image inspect` JSON.
///
/// Apple pretty-prints with spaces (`"key" : "value"`); docker/podman `--format` is compact
/// (`"key":"value"`). Match either so a fingerprint mismatch never falsely forces a rebuild.
fn parse_label_from_container_inspect(json: &str, key: &str) -> Option<String> {
  let key_pat = format!(r#""{key}""#);
  let key_at = json.find(&key_pat)?;
  let after_key = &json[key_at + key_pat.len()..];
  let colon = after_key.find(':')?;
  let after_colon = after_key[colon + 1..].trim_start();
  let rest = after_colon.strip_prefix('"')?;
  let end = rest.find('"')?;
  Some(rest[..end].to_string())
}

/// The host user's numeric UID/GID (via `id -u` / `id -g`), so the container's
/// `agent` user can own the files it writes into the mount. Falls back to
/// 1000:1000 if `id` is unavailable.
pub fn host_ids() -> (u32, u32) {
  (id_value("-u").unwrap_or(1000), id_value("-g").unwrap_or(1000))
}

fn id_value(flag: &str) -> Option<u32> {
  let out = std::process::Command::new("id").arg(flag).output().ok()?;
  out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten()
}

/// Status of one scsh image on the host runtime — for `scsh build-images` and the
/// dashboard's images panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageStatus {
  /// Display name: `base`, or the harness name (`opencode`, `claude`, …).
  pub name: String,
  pub tag: String,
  /// The image exists on the runtime, whatever its fingerprint.
  pub exists: bool,
  /// The image exists AND its fingerprint label matches the embedded Dockerfile.
  pub up_to_date: bool,
  /// Creation time (shortened RFC3339), when the runtime reports one (docker/podman).
  pub created: Option<String>,
  /// Human-readable size, when the runtime reports one (docker/podman).
  pub size: Option<String>,
}

/// One status row per scsh image: the shared base first, then every harness image,
/// each compared against the fingerprint the embedded Dockerfile would produce today.
pub fn image_statuses(runtime: &str) -> Vec<ImageStatus> {
  // Fingerprints must match what `run` / `build-images` actually build (Apple gets a
  // comment-stripped Dockerfile under the gRPC header limit).
  let df = dockerfile_for_runtime(runtime);
  let (uid, gid) = host_ids();
  let tz = host_timezone();
  let base_fp = base_image_fingerprint(&df, uid, gid, &tz);
  let mut out = vec![image_status_of(runtime, "base", BASE_IMAGE_TAG, &base_fp)];
  for h in crate::config::Harness::ALL {
    let spec = image_build_spec(h, &df, uid, gid, &tz);
    out.push(image_status_of(runtime, h.as_str(), &spec.tag, &spec.fingerprint));
  }
  out
}

fn image_status_of(runtime: &str, name: &str, tag: &str, expected_fp: &str) -> ImageStatus {
  let actual = image_inspect_fingerprint(runtime, tag);
  // A labelless (pre-scsh or hand-built) image answers inspect but yields no fingerprint —
  // it exists, it is just never up to date.
  let exists = actual.is_some() || image_exists(runtime, tag);
  let up_to_date = actual.as_deref() == Some(expected_fp);
  let (created, size) = if exists { image_created_size(runtime, tag) } else { (None, None) };
  ImageStatus { name: name.into(), tag: tag.into(), exists, up_to_date, created, size }
}

fn image_exists(runtime: &str, tag: &str) -> bool {
  std::process::Command::new(runtime)
    .args(["image", "inspect", tag])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

/// Creation time + human size for an image. docker/podman report both via `--format`;
/// Apple `container` has no such formatter, so both stay `None` there.
fn image_created_size(runtime: &str, tag: &str) -> (Option<String>, Option<String>) {
  if runtime == "container" {
    return (None, None);
  }
  let out =
    std::process::Command::new(runtime).args(["image", "inspect", tag, "--format", "{{.Created}}\t{{.Size}}"]).output();
  let Ok(out) = out else { return (None, None) };
  if !out.status.success() {
    return (None, None);
  }
  parse_created_size(&String::from_utf8_lossy(&out.stdout))
}

/// Parse one `{{.Created}}\t{{.Size}}` inspect line into (short timestamp, human size).
fn parse_created_size(line: &str) -> (Option<String>, Option<String>) {
  let mut parts = line.trim().splitn(2, '\t');
  let created = parts.next().filter(|s| !s.is_empty()).map(short_created);
  let size = parts.next().and_then(|s| s.trim().parse::<u64>().ok()).map(format_image_size);
  (created, size)
}

/// Trim an RFC3339 timestamp to whole seconds: `2026-07-05T01:02:03.123456789Z` →
/// `2026-07-05 01:02:03 UTC`. Anything shorter passes through unchanged.
fn short_created(ts: &str) -> String {
  if ts.len() >= 19 && ts.as_bytes()[10] == b'T' {
    format!("{} {} UTC", &ts[..10], &ts[11..19])
  } else {
    ts.to_string()
  }
}

/// Human-readable image size (base-1000, one decimal — matching `docker images` output).
fn format_image_size(bytes: u64) -> String {
  const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
  let mut value = bytes as f64;
  let mut unit = 0;
  while value >= 1000.0 && unit + 1 < UNITS.len() {
    value /= 1000.0;
    unit += 1;
  }
  if unit == 0 {
    format!("{bytes}B")
  } else {
    format!("{value:.1}{}", UNITS[unit])
  }
}

/// Build argv for the stdin method: the Dockerfile is sent on stdin (`-`). `no_cache` adds
/// `--no-cache` so a forced rebuild re-runs every layer instead of no-op'ing on the cache.
/// Prefer [`build_command_context`] when recording a TUI cast — a PTY cannot feed stdin the
/// same way `Proc::run_with_stdin` does.
pub fn build_command_stdin(
  runtime: &str, tag: &str, target: &str, uid: u32, gid: u32, tz: &str, fingerprint: &str, no_cache: bool,
) -> Vec<String> {
  let mut v = vec![runtime.into(), "build".into(), "-t".into(), tag.into(), "--target".into(), target.into()];
  if no_cache {
    v.push("--no-cache".into());
  }
  v.extend(build_args(uid, gid, tz));
  v.extend(build_labels(fingerprint));
  v.push("-".into());
  v
}

pub fn build_command_context(
  runtime: &str, tag: &str, target: &str, context_dir: &str, uid: u32, gid: u32, tz: &str, fingerprint: &str,
  no_cache: bool,
) -> Vec<String> {
  let mut v = vec![runtime.into(), "build".into(), "-t".into(), tag.into(), "--target".into(), target.into()];
  if no_cache {
    v.push("--no-cache".into());
  }
  v.extend(build_args(uid, gid, tz));
  v.extend(build_labels(fingerprint));
  v.push(context_dir.into());
  v
}

/// True when the host has an `asciinema` CLI that can record a build's PTY (the same
/// ASCII-cinema path skills use inside the container).
pub fn asciinema_available() -> bool {
  std::process::Command::new("asciinema")
    .arg("--version")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

/// Argv that records `build_shell_cmd` under a real PTY into `cast_path` (asciinema 3.x).
///
/// `--headless` keeps the live board's terminal free (the cast is the UI); `--return` forwards
/// the builder's exit status; `--overwrite` lets a retry reuse the same path. Window size matches
/// the default harness PTY so BuildKit / Apple `container` render their native progress TUI.
pub fn asciinema_rec_argv(cast_path: &str, cols: u16, rows: u16, build_shell_cmd: &str) -> Vec<String> {
  vec![
    "asciinema".into(),
    "rec".into(),
    "-q".into(),
    "--overwrite".into(),
    "--return".into(),
    "--headless".into(),
    "-f".into(),
    "asciicast-v3".into(),
    "--window-size".into(),
    format!("{cols}x{rows}"),
    "--command".into(),
    build_shell_cmd.into(),
    cast_path.into(),
  ]
}

/// Directory for host-recorded **build** casts (`$SCSH_HOME/casts`, default `~/.scsh/casts`).
/// Disposable: safe to clean whenever — a rebuild recreates them.
pub fn host_casts_dir() -> std::path::PathBuf {
  scsh_home().join("casts")
}

/// **Permanent** directory for skill-run recordings (`$SCSH_HOME/recordings`, default
/// `~/.scsh/recordings`). Deliberately separate from the cleanable build-cast dir
/// [`host_casts_dir`]: these are the recordings of actual agent runs — scsh never deletes
/// them, and nothing that treats `casts/` as scratch can take them along. Kept outside any
/// caller repo so a throwaway clone (e.g. code-beautiful-review) cannot delete them either.
pub fn host_recordings_dir() -> std::path::PathBuf {
  scsh_home().join("recordings")
}

/// Durable directory for preserved harness run logs (`$SCSH_HOME/logs`).
pub fn host_logs_dir() -> std::path::PathBuf {
  scsh_home().join("logs")
}

/// scsh's durable home on the host (`$SCSH_HOME`, else `~/.scsh`, else a temp fallback).
/// Same root the daemon store and build casts already use.
pub fn scsh_home() -> std::path::PathBuf {
  if let Some(dir) = std::env::var_os("SCSH_HOME").filter(|s| !s.is_empty()) {
    return std::path::PathBuf::from(dir);
  }
  match std::env::var_os("HOME").filter(|s| !s.is_empty()) {
    Some(home) => std::path::PathBuf::from(home).join(".scsh"),
    None => std::env::temp_dir().join("scsh-home"),
  }
}

/// One harness image scsh may build from the shared Dockerfile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBuildSpec {
  pub harness: Harness,
  pub tag: String,
  pub target: String,
  pub fingerprint: String,
}

pub fn image_build_spec(harness: Harness, dockerfile: &str, uid: u32, gid: u32, tz: &str) -> ImageBuildSpec {
  let target = image_target(harness);
  ImageBuildSpec {
    harness,
    tag: image_tag(harness),
    target: target.to_string(),
    fingerprint: image_build_fingerprint(dockerfile, target, uid, gid, tz),
  }
}

/// True when the runtime exposes `buildx bake` (multi-target build in one command).
#[allow(dead_code)] // retained for tests; runs build base then per-harness instead.
pub fn runtime_supports_bake(runtime: &str) -> bool {
  std::process::Command::new(runtime)
    .args(["buildx", "version"])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

/// Build argv for one `buildx bake` that tags every listed harness target.
#[allow(dead_code)] // retained for tests; runs build base then per-harness instead.
pub fn build_command_bake(runtime: &str, bake_targets: &[String]) -> Vec<String> {
  let mut v = vec![runtime.into(), "buildx".into(), "bake".into(), "--load".into(), "-f".into(), "-".into()];
  v.extend(bake_targets.iter().cloned());
  v
}

/// JSON bake definition: one context dir, multiple Dockerfile `--target`s sharing `scsh-base`.
#[allow(dead_code)] // retained for tests; runs build base then per-harness instead.
pub fn bake_definition_json(context_dir: &str, specs: &[ImageBuildSpec], uid: u32, gid: u32, tz: &str) -> String {
  use crate::json::quote;
  let mut entries = Vec::with_capacity(specs.len());
  for spec in specs {
    entries.push(format!(
      r#"    {}: {{
      "context": {},
      "dockerfile": "Dockerfile",
      "target": {},
      "tags": [{}],
      "args": {{
        "AGENT_UID": "{uid}",
        "AGENT_GID": "{gid}",
        "TZ": {}
      }},
      "labels": {{
        {}: {}
      }}
    }}"#,
      quote(&spec.target),
      quote(context_dir),
      quote(&spec.target),
      quote(&spec.tag),
      quote(tz),
      quote(BUILD_FINGERPRINT_LABEL),
      quote(&spec.fingerprint),
    ));
  }
  format!("{{\n  \"target\": {{\n{}\n  }}\n}}", entries.join(",\n"))
}

/// How the caller repo reaches the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoMountMode {
  /// Linux-friendly path: bind-mount the host run dir at [`AGENT_REPO`].
  Full,
  /// macOS Apple Container: repo is cloned inside the container from a host git daemon;
  /// only the gitignored `tmp/` tree is bind-mounted for results and forwarded auth.
  TmpOnly,
}

/// Run argv: run the freshly built image, removing the container afterwards. For
/// rootless podman, `--userns=keep-id` maps the host UID to the same UID inside the
/// container so the `agent` user can read/write the mount; docker (and Apple
/// `container`) map the UID directly and need no such flag.
pub fn run_command(
  runtime: &str, tag: &str, run_dir: &str, name: &str, env: &[(String, String)], volumes: &[(&str, &str)],
  command: &str, repo_mount: RepoMountMode,
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
  match repo_mount {
    RepoMountMode::Full => {
      v.push("-v".into());
      v.push(format!("{run_dir}:{AGENT_REPO}"));
    }
    RepoMountMode::TmpOnly => {
      v.push("-v".into());
      v.push(format!("{run_dir}/tmp:{AGENT_REPO}/tmp"));
    }
  }
  v.push(tag.into());
  v.push("/bin/sh".into());
  v.push("-c".into());
  v.push(command.into());
  v
}

/// True when a named container still exists (running or stopped) for the given runtime.
/// docker, podman, and Apple `container` all support `inspect <name>`, but Apple's exits 0
/// with an empty `[]` for a missing container — so require a non-empty JSON result too.
pub fn container_named_exists(runtime: &str, name: &str) -> bool {
  use std::process::{Command, Stdio};
  if name.is_empty() {
    return false;
  }
  let Ok(out) = Command::new(runtime).args(["inspect", name]).stderr(Stdio::null()).output() else {
    return false;
  };
  if !out.status.success() {
    return false;
  }
  let body = String::from_utf8_lossy(&out.stdout);
  let body = body.trim();
  !(body.is_empty() || body == "[]" || body == "null")
}

/// Probe every runtime scsh might use — for orphan prune jobs with no runtime recorded.
pub fn container_named_exists_any(name: &str) -> bool {
  for rt in runtime_candidates(cfg!(target_os = "macos")) {
    if which(rt).is_some() && container_named_exists(rt, name) {
      return true;
    }
  }
  false
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

/// If the opencode provider that `model` uses logs in via an OAuth token that has EXPIRED, return
/// that provider's name. An expired OAuth login (e.g. the ChatGPT-plan `openai` provider) makes
/// opencode silently stop responding — the run would hang instead of failing — so scsh checks the
/// `expires` in `~/.local/share/opencode/auth.json` up front and refuses with a re-login message.
/// `None` when the provider uses a non-expiring API key, is absent, or the token is still valid.
pub fn opencode_expired_provider(model: &str) -> Option<String> {
  let provider = opencode_model_provider(model);
  let path = opencode_auth_in(std::env::var_os("XDG_DATA_HOME").as_deref(), std::env::var_os("HOME").as_deref())?;
  let crate::json::Value::Object(obj) = crate::json::parse(&std::fs::read_to_string(&path).ok()?).ok()? else {
    return None;
  };
  let crate::json::Value::Object(fields) = obj.iter().find(|(k, _)| k == provider).map(|(_, v)| v)? else {
    return None;
  };
  let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v);
  // Only OAuth logins expire; a `type: "api"` static key never does.
  match field("type") {
    Some(crate::json::Value::String(s)) if s == "oauth" => {}
    _ => return None,
  }
  let expires_ms = match field("expires") {
    Some(crate::json::Value::Number(n)) => *n,
    _ => return None,
  };
  let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
  (expires_ms < now_ms as f64).then(|| provider.to_string())
}

pub fn claude_oauth_token() -> Option<String> {
  std::env::var(CLAUDE_OAUTH_TOKEN_ENV).ok().filter(|s| !s.is_empty())
}

fn claude_credentials_file_on_host() -> Option<PathBuf> {
  let home = std::env::var_os("HOME")?;
  let path = PathBuf::from(home).join(".claude").join(".credentials.json");
  path.is_file().then_some(path)
}

/// Whether the host has credentials containers can use: `CLAUDE_CODE_OAUTH_TOKEN`,
/// `~/.claude/.credentials.json`, or the macOS login keychain.
pub fn claude_container_auth_ready() -> bool {
  claude_oauth_token().is_some()
    || claude_credentials_file_on_host().is_some()
    || claude_keychain_credentials_json().is_some()
}

pub fn check_harness_host(harness: Harness) -> Result<(), String> {
  match harness {
    Harness::Opencode => {
      if opencode_auth_ready() {
        Ok(())
      } else {
        Err("opencode harness unavailable (auth not found at ~/.local/share/opencode/auth.json — run `opencode auth login`)".into())
      }
    }
    Harness::Claude => {
      if claude_container_auth_ready() {
        Ok(())
      } else {
        Err(
          "claude harness unavailable (no CLAUDE_CODE_OAUTH_TOKEN, no ~/.claude/.credentials.json, and no macOS \
           keychain credentials — log in with `claude`, or run `claude setup-token` and export CLAUDE_CODE_OAUTH_TOKEN)"
            .into(),
        )
      }
    }
    Harness::Codex => {
      if codex_container_auth_ready() {
        Ok(())
      } else {
        Err(
          "codex harness unavailable (no ~/.codex/auth.json and OPENAI_API_KEY is not set \
           — run `codex login`, or export OPENAI_API_KEY in your shell)"
            .into(),
        )
      }
    }
    Harness::Grok => {
      if !grok_container_auth_ready() {
        Err(
          "grok harness unavailable (no ~/.grok/auth.json and XAI_API_KEY is not set \
           — run `grok login` (or `grok login --device-auth`), or export XAI_API_KEY in your shell)"
            .into(),
        )
      } else if grok_auth_expired() {
        // The interactive Build TUI can't refresh a lapsed session non-interactively — it would
        // demand a browser sign-in that can't be clicked inside the container.
        Err(
          "grok login has expired — run `grok` on the host and sign in, then re-run \
           (its interactive session must be refreshed on the host; the container can't do it)"
            .into(),
        )
      } else {
        Ok(())
      }
    }
    Harness::Cursor => {
      if cursor_container_auth_ready() {
        Ok(())
      } else {
        Err(
          "cursor harness unavailable (no cursor auth on host — run `cursor agent login`, \
           or export CURSOR_API_KEY in your shell)"
            .into(),
        )
      }
    }
  }
}

/// Host-side opencode model list, loaded once per `scsh run` when needed.
pub struct OpencodeModelProbe {
  available: Option<std::collections::HashSet<String>>,
}

impl OpencodeModelProbe {
  /// Run `opencode models <provider>` for each provider required by **selected** skills'
  /// explicit opencode models (profile-scoped — not every model in `.scsh.yml`).
  pub fn for_selected(skills: &[crate::config::ResolvedInvocation]) -> Self {
    let requested = requested_opencode_models(skills);
    if requested.is_empty() {
      return Self { available: None };
    }
    if which("opencode").is_none() || !opencode_auth_ready() {
      return Self { available: None };
    }
    Self { available: Some(load_opencode_models_for(&requested).unwrap_or_default()) }
  }

  pub fn check_model(&self, model: &str) -> Result<(), String> {
    match &self.available {
      Some(set) if set.contains(model) => Ok(()),
      Some(_) => Err(format!("opencode model '{model}' not listed by `opencode models` on this host")),
      None => Ok(()),
    }
  }
}

/// Explicit opencode `model:` values on selected invocations (deduplicated).
fn requested_opencode_models(skills: &[crate::config::ResolvedInvocation]) -> std::collections::HashSet<String> {
  skills
    .iter()
    .filter(|s| s.harness == Harness::Opencode)
    .filter_map(|s| s.model.as_deref())
    .map(str::to_string)
    .collect()
}

/// Provider segment of an opencode model id (`openai/gpt-5.5` → `openai`).
fn opencode_model_provider(model: &str) -> &str {
  model.split('/').next().unwrap_or(model)
}

/// Unique providers for a set of requested model ids, stable order.
fn opencode_providers_for_models(models: &std::collections::HashSet<String>) -> Vec<String> {
  let mut providers: Vec<String> = models.iter().map(|m| opencode_model_provider(m).to_string()).collect();
  providers.sort_unstable();
  providers.dedup();
  providers
}

/// Harness auth plus, for opencode skills with an explicit `model:`, a host `opencode models` check.
pub fn check_skill_host(harness: Harness, model: Option<&str>, probe: &OpencodeModelProbe) -> Result<(), String> {
  check_harness_host(harness)?;
  if harness == Harness::Opencode {
    if let Some(m) = model {
      // An expired provider login would make opencode hang; surface it as a clear "log in again".
      if let Some(provider) = opencode_expired_provider(m) {
        return Err(format!(
          "opencode's '{provider}' login has expired — run `opencode auth login` on the host \
           (choose {provider}), then re-run"
        ));
      }
      probe.check_model(m)?;
    }
  }
  Ok(())
}

fn load_opencode_models_for(
  requested: &std::collections::HashSet<String>,
) -> Result<std::collections::HashSet<String>, String> {
  let mut all = std::collections::HashSet::new();
  for provider in opencode_providers_for_models(requested) {
    let output = std::process::Command::new("opencode")
      .args(["models", &provider])
      .output()
      .map_err(|e| format!("could not run `opencode models {provider}`: {e}"))?;
    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      let detail = stderr.trim();
      let msg =
        if detail.is_empty() { format!("opencode models {provider} exited with an error") } else { detail.to_string() };
      return Err(msg);
    }
    all.extend(parse_opencode_models(&String::from_utf8_lossy(&output.stdout)));
  }
  Ok(all)
}

fn parse_opencode_models(stdout: &str) -> std::collections::HashSet<String> {
  stdout.lines().map(|line| line.trim()).filter(|line| !line.is_empty()).map(|line| line.to_string()).collect()
}

/// Volume mounts for a harness run. NONE of the harnesses need extra bind-mounts: every one
/// COPIES its auth/config into the run clone's gitignored `tmp/` (the images'
/// `CLAUDE_CONFIG_DIR` / `CODEX_HOME` / `GROK_HOME` / `CURSOR_CONFIG_DIR`, and opencode's
/// `XDG_DATA_HOME` / `XDG_CONFIG_HOME`), which rides along with the repo mount in both mount
/// modes — so no single-file bind mount (rejected by Docker Desktop on macOS) is ever used.
pub fn harness_volumes(_harness: Harness) -> Vec<(String, String)> {
  Vec::new()
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
  let filled =
    std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf)).is_ok();
  if !filled {
    let nanos =
      std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
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
// Repo sync: host push IN, host fetch OUT (never GitHub from inside the container)
// ---------------------------------------------------------------------------

/// Bare repo directory name under a run dir — host `git push` target for push IN.
pub const TRANSPORT_BARE: &str = "transport.git";

/// Bare repo directory name under a run dir — container `git push` target for pull OUT.
pub const PULL_BARE: &str = "pull.git";

/// Container env: optional override for the host address of the per-run `git daemon`.
/// When unset, the container entry resolves it from `ip route` (default gateway).
pub const GIT_TRANSPORT_HOST_ENV: &str = "SCSH_GIT_HOST";

/// Container env: port for the per-run `git daemon`.
pub const GIT_TRANSPORT_PORT_ENV: &str = "SCSH_GIT_PORT";

/// Shell snippet run inside the container: host IP for the per-run git daemon.
/// Uses the container's default-route gateway (vmnet bridge on Apple Container).
/// `SCSH_GIT_HOST` overrides when set.
pub const GIT_TRANSPORT_HOST_SHELL: &str =
  "host=${SCSH_GIT_HOST:-$(ip -4 route show default 2>/dev/null | awk '{print $3; exit}')}";

/// Shell guard: fail fast when the gateway cannot be determined.
pub const GIT_TRANSPORT_HOST_GUARD: &str =
  "[ -n \"$host\" ] || { echo \"scsh: could not determine host gateway for git transport (set SCSH_GIT_HOST)\" >&2; exit 1; }";

/// Whether scsh moves git state via local push/fetch + git daemon instead of bind-mounting
/// `.git` across macOS→Linux (Apple Container). On macOS Apple Container this is always
/// enabled — bind-mounting `.git` corrupts objects. Elsewhere override with `SCSH_GIT_TRANSPORT=0|1`.
pub fn uses_git_transport(runtime: &str) -> bool {
  if cfg!(target_os = "macos") && runtime == "container" {
    return true;
  }
  match std::env::var("SCSH_GIT_TRANSPORT").ok().as_deref() {
    Some("0") | Some("false") => false,
    Some("1") | Some("true") => true,
    _ => false,
  }
}

/// Pick a free TCP port on all interfaces for a short-lived `git daemon`.
pub fn pick_ephemeral_port() -> Result<u16, String> {
  use std::net::TcpListener;
  let listener = TcpListener::bind("0.0.0.0:0").map_err(|e| format!("could not bind an ephemeral port: {e}"))?;
  listener.local_addr().map(|a| a.port()).map_err(|e| format!("could not read ephemeral port: {e}"))
}

/// `git clone` argv: host-side push IN when bind-mounting (Linux host → Linux container).
pub fn clone_command(src: &str, dst: &str) -> Vec<String> {
  vec!["git".into(), "clone".into(), src.into(), dst.into()]
}

/// `git fsck` argv: verify clone integrity after host-side push IN / clone.
pub fn fsck_command(repo: &str) -> Vec<String> {
  vec!["git".into(), "-C".into(), repo.into(), "fsck".into(), "--no-progress".into()]
}

/// Create an empty bare repository at `path` (parent dirs created as needed).
pub fn init_bare_repo(path: &Path) -> Result<(), String> {
  if path.is_dir() {
    return Ok(());
  }
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
  }
  use std::process::Command;
  Command::new("git")
    .args(["init", "--bare"])
    .arg(path)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
    .then_some(())
    .ok_or_else(|| format!("git init --bare failed for {}", path.display()))
}

/// Host push IN: mirror every local `refs/heads/*` branch into the bare transport repo.
/// Code-review prep uses `git branch -f main <base>`; pushing heads (not stale
/// `refs/remotes/origin/*`) ensures `origin/main..HEAD` resolves inside the container.
pub fn push_transport_refs(root: &Path, bare: &Path) -> Result<(), String> {
  init_bare_repo(bare)?;
  let bare_s = bare.to_string_lossy();
  let Some(heads) = git_stdout(root, &["for-each-ref", "--format=%(refname)", "refs/heads"]) else {
    return Err(format!("could not read local branches in {}", root.display()));
  };
  let mut pushed = false;
  for line in heads.lines() {
    let refname = line.trim();
    if refname.is_empty() {
      continue;
    }
    let spec = format!("{refname}:{refname}");
    if !git_ok(root, &["push", "--quiet", &bare_s, &spec]) {
      return Err(format!("git push {refname} to {} failed", bare.display()));
    }
    pushed = true;
  }
  if !pushed {
    return Err(format!("no local branches to push from {}", root.display()));
  }
  if let Some(branch) = git_stdout(root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
    let branch = branch.trim();
    if !branch.is_empty() && branch != "HEAD" {
      let head_ref = format!("refs/heads/{branch}");
      if !git_bare_ok(bare, &["symbolic-ref", "HEAD", &head_ref]) {
        return Err(format!("could not set HEAD on {}", bare.display()));
      }
    }
  }
  Ok(())
}

/// Commit a single file into a bare repo's checked-out branch, on top of HEAD, so a container
/// that clones the transport sees it in its working tree. scsh uses this to place a harness
/// definition's `SKILL.md` into the Apple Container git-transport clone without dirtying the
/// caller's repo. `commit_name`/`commit_email` identify the scaffolding commit (it lives only
/// in the throwaway transport repo and is never returned to the caller).
pub fn inject_file_into_bare(
  bare: &Path, rel_path: &str, content: &str, commit_name: &str, commit_email: &str,
) -> Result<(), String> {
  use std::io::Write;
  use std::process::{Command, Stdio};

  // Commits land on the branch HEAD points at, so the default clone checkout includes the file.
  let head_ref = git_stdout(bare, &["symbolic-ref", "HEAD"])
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .ok_or_else(|| format!("could not read HEAD of {}", bare.display()))?;

  // 1. Write the file content as a blob.
  let mut child = Command::new("git")
    .arg("-C")
    .arg(bare)
    .args(["hash-object", "-w", "--stdin"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|e| format!("git hash-object: {e}"))?;
  child
    .stdin
    .take()
    .ok_or("git hash-object: no stdin")?
    .write_all(content.as_bytes())
    .map_err(|e| format!("git hash-object write: {e}"))?;
  let out = child.wait_with_output().map_err(|e| format!("git hash-object: {e}"))?;
  if !out.status.success() {
    return Err("git hash-object failed".into());
  }
  let blob = String::from_utf8_lossy(&out.stdout).trim().to_string();

  // 2. Build a tree = HEAD's tree + the new file, using a throwaway index (a bare repo has none).
  let index = bare.join("scsh-inject.index");
  let git_index = |args: &[&str]| -> Result<std::process::Output, String> {
    Command::new("git")
      .arg("-C")
      .arg(bare)
      .env("GIT_INDEX_FILE", &index)
      .args(args)
      .output()
      .map_err(|e| format!("git {}: {e}", args.join(" ")))
  };
  let build_tree = || -> Result<String, String> {
    let read = git_index(&["read-tree", &head_ref])?;
    if !read.status.success() {
      return Err("git read-tree failed".into());
    }
    let cacheinfo = format!("100644,{blob},{rel_path}");
    let add = git_index(&["update-index", "--add", "--cacheinfo", &cacheinfo])?;
    if !add.status.success() {
      return Err("git update-index failed".into());
    }
    let tree = git_index(&["write-tree"])?;
    if !tree.status.success() {
      return Err("git write-tree failed".into());
    }
    Ok(String::from_utf8_lossy(&tree.stdout).trim().to_string())
  };
  let tree = build_tree();
  let _ = std::fs::remove_file(&index); // clean up the throwaway index whatever happened
  let tree = tree?;

  // 3. Commit the tree on top of HEAD and move the branch to it. commit-tree needs an identity,
  // which the bare repo lacks, so pass it explicitly.
  let commit_out = Command::new("git")
    .arg("-C")
    .arg(bare)
    .args(["commit-tree", &tree, "-p", &head_ref, "-m", "scsh: add harness-definition skill body"])
    .env("GIT_AUTHOR_NAME", commit_name)
    .env("GIT_AUTHOR_EMAIL", commit_email)
    .env("GIT_COMMITTER_NAME", commit_name)
    .env("GIT_COMMITTER_EMAIL", commit_email)
    .output()
    .map_err(|e| format!("git commit-tree: {e}"))?;
  if !commit_out.status.success() {
    return Err("git commit-tree failed".into());
  }
  let commit = String::from_utf8_lossy(&commit_out.stdout).trim().to_string();

  if !git_ok(bare, &["update-ref", &head_ref, &commit]) {
    return Err(format!("could not update {head_ref} in {}", bare.display()));
  }
  Ok(())
}

/// Path scsh fetches commits from after a run: the run clone, or `pull.git` when git transport
/// moved the repo only inside the container.
pub fn commits_fetch_path(run_dir: &Path) -> PathBuf {
  let pull = run_dir.join(PULL_BARE);
  if pull.is_dir() {
    pull
  } else {
    run_dir.to_path_buf()
  }
}

/// Shell wrapper run inside the container before the harness: clone from the host git daemon,
/// materialize `origin/*` locals, optionally set commit identity, run the harness, optionally
/// push commits back to the host bare `pull.git`.
pub fn git_transport_entry(harness: &str, push_commits: bool, commit_name: &str, commit_email: &str) -> String {
  let mut script = format!(
    "set -e\n\
     {host_shell}\n\
     {host_guard}\n\
     git clone \"git://${{host}}:${{{port}}}/transport.git\" /home/agent/.scsh-clone\n\
     (cd /home/agent/.scsh-clone && tar -cf - .) | (cd {repo} && tar -xf -)\n\
     rm -rf /home/agent/.scsh-clone\n\
     cd {repo}\n\
     mkdir -p {repo}/tmp\n\
     git rev-parse --verify origin/main >/dev/null 2>&1 || {{ echo \"scsh: origin/main missing after git transport clone (point local main at the review base)\" >&2; exit 1; }}\n\
     cur=$(git rev-parse --abbrev-ref HEAD)\n\
     for ref in $(git for-each-ref --format='%(refname:short)' refs/remotes/origin); do\n\
       branch=${{ref#origin/}}\n\
       [ \"$branch\" = HEAD ] && continue\n\
       [ \"$branch\" = \"$cur\" ] && continue\n\
       git branch --force \"$branch\" \"origin/$branch\" >/dev/null 2>&1 || true\n\
     done\n",
    host_shell = GIT_TRANSPORT_HOST_SHELL,
    host_guard = GIT_TRANSPORT_HOST_GUARD,
    port = GIT_TRANSPORT_PORT_ENV,
    repo = AGENT_REPO,
  );
  if push_commits {
    script.push_str(&format!(
      "git config user.email {}\ngit config user.name {}\n",
      shell_quote(commit_email),
      shell_quote(commit_name),
    ));
  }
  script.push_str(harness);
  if push_commits {
    script.push_str("\ngit push \"git://${host}:${SCSH_GIT_PORT}/pull.git\" HEAD");
  }
  script
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
  use std::process::Command;
  Command::new("git")
    .arg("-C")
    .arg(dir)
    .args(args)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

fn git_bare_ok(bare: &Path, args: &[&str]) -> bool {
  use std::process::Command;
  Command::new("git")
    .arg("--git-dir")
    .arg(bare)
    .args(args)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
  use std::process::Command;
  let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
  out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
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
    // macOS: Apple 'container' ONLY — no auto Docker/Podman fallback.
    assert_eq!(runtime_candidates(true), &["container"]);
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
  fn macos_never_auto_falls_back_to_docker() {
    // Docker present but Apple 'container' absent → macOS auto-detects NOTHING (no silent
    // Docker fallback). The user must opt in with SCSH_RUNTIME=docker.
    let d = tmp("detect-macos-nofallback");
    make_exec(&d.join("docker"));
    make_exec(&d.join("podman"));
    let path = OsString::from(d.to_str().unwrap());
    assert!(detect_runtime_in(true, &path).is_none(), "no auto Docker/Podman fallback on macOS");
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
    assert_eq!(image_tag(Harness::Codex), "scsh-codex:latest");
    assert_eq!(image_tag(Harness::Grok), "scsh-grok:latest");
    assert_eq!(image_tag(Harness::Cursor), "scsh-cursor:latest");
  }

  #[test]
  fn dockerfile_has_shared_base_and_harness_targets() {
    let df = dockerfile();
    assert!(df.contains("FROM debian:bookworm-slim AS scsh-base"));
    assert!(df.contains("FROM scsh-base AS scsh-opencode"));
    assert!(df.contains("FROM scsh-base AS scsh-claude"));
    assert!(df.contains("FROM scsh-base AS scsh-codex"));
    assert!(df.contains("npm install -g opencode-ai"));
    assert!(df.contains("npm install -g @anthropic-ai/claude-code"));
    assert!(df.contains("npm install -g @openai/codex"));
    assert!(!df.contains("CMD ["));
    assert!(df.contains("ENV SCSH_RUN_LOG=/home/agent/repo/tmp/scsh-run.log"));
    assert!(df.contains("ENV SCSH=1"));
  }

  /// Apple Containers sends the Dockerfile in a gRPC header (16KB default). Files that
  /// approach that limit fail with "Stream unexpectedly closed" (apple/container#735).
  /// Keep a margin under the soft limit so builds keep working on macOS / Apple Silicon.
  #[test]
  fn dockerfile_stays_under_apple_containers_grpc_header_limit() {
    let bytes = include_str!("Dockerfile").len();
    assert!(
      bytes < APPLE_CONTAINER_DOCKERFILE_SOFT_LIMIT,
      "src/Dockerfile is {bytes} bytes; Apple Containers fails builds near \
       {APPLE_CONTAINER_DOCKERFILE_HARD_LIMIT} bytes (apple/container#735). \
       Trim comments or split stages before growing further."
    );
    let compact = compact_dockerfile_for_apple(include_str!("Dockerfile"));
    assert!(
      compact.len() < APPLE_CONTAINER_DOCKERFILE_HARD_LIMIT,
      "compacted Dockerfile is {} bytes — still over Apple's hard limit",
      compact.len()
    );
    // Compaction must preserve every stage and the TUI recorder body.
    assert!(compact.contains("FROM scsh-base AS scsh-cursor"));
    assert!(compact.contains("pane_relaunch="));
    assert!(compact.contains("# syntax=docker/dockerfile:1"));
  }

  #[test]
  fn compact_dockerfile_strips_comments_but_keeps_heredoc_code() {
    let src = "\
# syntax=docker/dockerfile:1
# long prose that must go
FROM scratch AS scsh-base
# section note
RUN cat > /bin/x <<'TAG'
#!/bin/sh
# keep shebang above; drop this comment
echo hi
TAG
# trailing
";
    let out = compact_dockerfile_for_apple(src);
    assert!(out.contains("# syntax=docker/dockerfile:1"));
    assert!(!out.contains("long prose"));
    assert!(!out.contains("section note"));
    assert!(!out.contains("trailing"));
    assert!(out.contains("#!/bin/sh"));
    assert!(!out.contains("drop this comment"));
    assert!(out.contains("echo hi"));
    assert!(out.contains("FROM scratch AS scsh-base"));
  }

  #[test]
  fn apple_dockerfile_size_gate_and_failure_rewrite() {
    assert!(!apple_dockerfile_too_large("small"));
    let big = "x".repeat(APPLE_CONTAINER_DOCKERFILE_HARD_LIMIT);
    assert!(apple_dockerfile_too_large(&big));
    let msg = apple_dockerfile_too_large_message(big.len());
    assert!(msg.contains("apple/container/issues/735"));
    assert!(msg.contains("SCSH_RUNTIME=docker"));

    let rewritten = rewrite_apple_build_failure("Error: unavailable: \"Stream unexpectedly closed.\"");
    assert!(rewritten.unwrap().contains("container builder delete --force"));
    assert!(rewrite_apple_build_failure("apt-get failed").is_none());
  }

  #[test]
  fn dockerfile_for_runtime_compacts_only_apple_container() {
    let raw = dockerfile();
    let apple = dockerfile_for_runtime("container");
    let docker = dockerfile_for_runtime("docker");
    assert_eq!(docker, raw);
    assert!(apple.len() <= raw.len());
    assert!(apple.contains("FROM debian:bookworm-slim AS scsh-base"));
  }

  #[test]
  fn dockerfile_codex_stage_points_codex_home_into_repo_tmp() {
    let df = dockerfile();
    assert!(df.contains("ENV CODEX_HOME=/home/agent/repo/tmp/.codex"));
    assert!(df.contains("codex --version"));
  }

  #[test]
  fn dockerfile_grok_stage_points_grok_home_into_repo_tmp() {
    let df = dockerfile();
    assert!(df.contains("FROM scsh-base AS scsh-grok"));
    assert!(df.contains("npm install -g @xai-official/grok"));
    assert!(df.contains("ENV GROK_HOME=/home/agent/repo/tmp/.grok"));
    assert!(df.contains("grok --version"));
  }

  #[test]
  fn dockerfile_cursor_stage_points_cursor_home_into_repo_tmp() {
    let df = dockerfile();
    assert!(df.contains("FROM scsh-base AS scsh-cursor"));
    assert!(df.contains("downloads.cursor.com/lab/"));
    assert!(df.contains("ENV CURSOR_AGENT_HOME=/usr/local/share/cursor-agent"));
    assert!(df.contains("mv \"$tmp/dist-package\" \"$CURSOR_AGENT_HOME\""));
    assert!(df.contains("ENV CURSOR_CONFIG_DIR=/home/agent/repo/tmp/.cursor"));
    assert!(df.contains("ENV XDG_CONFIG_HOME=/home/agent/repo/tmp/.config"));
    assert!(df.contains("cursor-agent --version"));
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
  fn dockerfile_tui_recorder_relaunches_and_quits_gracefully() {
    let df = dockerfile();
    // A harness that dies without its result file is relaunched in the same pane (a stray
    // SIGTERM has killed cursor-agent ~1.5s in); a harness that ignores its quit keys is
    // waited on and re-asked before the session is force-killed, so `.exit` still gets written.
    assert!(df.contains("pane_relaunch="), "relaunch loop missing from scsh-tui-record");
    assert!(df.contains(r#"[ -f \"$result\" ] || [ \$n -ge 2 ]"#), "relaunch stop conditions missing");
    assert!(df.contains("re-send $quit"), "graceful quit re-send missing");
    assert!(df.contains("killed session (harness ignored quit)"), "force-kill fallback missing");
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
      df.contains(&format!("mkdir -p {AGENT_REPO}/tmp")),
      "image must create the bind-mounted tmp/ so logs/results have a home before the mount attaches"
    );
    assert!(
      df.contains("mkdir -p \"$(dirname \"$SCSH_RUN_LOG\")\""),
      "scsh-tui-record must mkdir the run-log parent before writing tuidebug/cast"
    );
    assert!(
      df.contains(&format!("ENV XDG_DATA_HOME={AGENT_REPO}/tmp/.xdg-data")),
      "XDG_DATA_HOME must live under the repo-mounted tmp/"
    );
    assert!(
      df.contains(&format!("ENV XDG_CONFIG_HOME={AGENT_REPO}/tmp/.config")),
      "XDG_CONFIG_HOME must live under the repo-mounted tmp/ (opencode config rides the mount)"
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
    assert!(OPENCODE_DATA_REL.starts_with("tmp/") && RUN_LOG_REL.starts_with("tmp/"));
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

  /// Harness install RUNs run as root and may scribble into /home/agent (npm cache,
  /// cursor-agent --version → ~/.cursor). Without a chown, USER agent cannot create
  /// $HOME/.cursor/projects for the workspace-trust marker (session kuovup failure).
  #[test]
  fn dockerfile_reclaims_agent_home_after_harness_installs() {
    let df = dockerfile();
    let chowns = df.matches("chown -R agent:agent /home/agent").count();
    assert!(chowns >= 5, "each harness stage must chown /home/agent after its install (found {chowns})");
    // Version probes must not write into the real agent home as root.
    for cli in ["opencode", "claude", "codex", "grok", "cursor-agent"] {
      assert!(df.contains(&format!("HOME=/tmp {cli} --version")), "{cli} --version must run with HOME=/tmp");
    }
  }

  #[test]
  fn skill_prompt_clause_names_global_skills_where_the_cli_discovers_them() {
    // Repo delivery: the committed path, for every harness.
    assert_eq!(skill_prompt_clause(Harness::Claude, "add", false), "Run the skill defined in .skills/add/SKILL.md.");
    // Global install on a natively-discovering CLI: invoked by NAME (/<name>).
    let claude = skill_prompt_clause(Harness::Claude, "code-review", true);
    assert!(claude.contains("globally installed skill named 'code-review'"), "got: {claude}");
    assert!(claude.contains("/code-review"), "got: {claude}");
    assert!(!claude.contains(".skills/"), "got: {claude}");
    let cursor = skill_prompt_clause(Harness::Cursor, "code-review", true);
    assert!(cursor.contains("globally installed skill named 'code-review'"), "got: {cursor}");
    // Global install elsewhere: referenced by its container path (under the mounted tmp/).
    for h in [Harness::Opencode, Harness::Codex, Harness::Grok] {
      let clause = skill_prompt_clause(h, "greet", true);
      assert_eq!(clause, "Run the skill defined in tmp/.scsh-skills/greet/SKILL.md.", "harness {h:?}");
    }
  }

  #[test]
  fn harness_command_global_skill_prompts() {
    let term = crate::config::Terminal::default();
    let claude = harness_command(Harness::Claude, Some("sonnet"), None, "code-review", "tmp/r.json", term, true);
    assert!(claude.contains("globally installed skill named"), "got: {claude}");
    assert!(!claude.contains(".skills/code-review"), "got: {claude}");
    let opencode = harness_command(Harness::Opencode, Some("openai/gpt-5.5"), None, "greet", "tmp/r.json", term, true);
    assert!(opencode.contains("tmp/.scsh-skills/greet/SKILL.md"), "got: {opencode}");
  }

  #[test]
  fn harness_command_builds_opencode_invocation() {
    // opencode now runs as a recorded interactive TUI (`opencode --prompt`), not headless.
    let cmd = harness_command(
      Harness::Opencode,
      Some("openai/gpt-5.5"),
      None,
      "add",
      "tmp/add.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(cmd.contains("scsh: harness=opencode"));
    assert!(cmd.contains("mkdir -p \"$(dirname \"${SCSH_RUN_LOG}\")\""), "got: {cmd}");
    assert!(cmd.contains("scsh-tui-record 200 50 double-ctrl-c enter tmp/add.json "), "got: {cmd}");
    assert!(cmd.contains("opencode -m openai/gpt-5.5 --prompt "), "got: {cmd}");
    assert!(!cmd.contains(" run "), "no headless run subcommand: {cmd}");
    assert!(cmd.contains(".skills/add/SKILL.md"));
    assert!(cmd.contains("SCSH_RESULT"));
    assert!(cmd.ends_with("2>&1 | tee \"${SCSH_RUN_LOG}\""));
    // Model-less: no -m flag, still the TUI.
    let bare = harness_command(
      Harness::Opencode,
      None,
      None,
      "multiply",
      "tmp/mul.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(bare.contains("opencode --prompt "), "got: {bare}");
    assert!(!bare.contains(" -m "), "got: {bare}");
  }

  #[test]
  fn harness_command_builds_claude_invocation() {
    let cmd = harness_command(
      Harness::Claude,
      Some("sonnet"),
      None,
      "add",
      "tmp/add_claude_sonnet_4_6_result.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(cmd.contains(".skills/add/SKILL.md"));
    // Interactive TUI recorded via scsh-tui-record (no inline shell, no screen-scraping).
    // bypassPermissions enables every tool; its consent screen is suppressed by the minimal
    // forwarded .claude.json (host-side), not by any flag here.
    assert!(
      cmd.contains("scsh-tui-record 200 50 slash-exit none tmp/add_claude_sonnet_4_6_result.json "),
      "got: {cmd}"
    );
    assert!(cmd.contains("claude --permission-mode bypassPermissions --model sonnet"), "got: {cmd}");
    assert!(!cmd.contains("--settings"), "got: {cmd}");
    assert!(!cmd.contains("claude -p"), "got: {cmd}");
    assert!(!cmd.contains("capture-pane"), "got: {cmd}");
    assert!(!cmd.contains("send-keys"), "got: {cmd}");
    assert!(cmd.ends_with("2>&1 | tee \"${SCSH_RUN_LOG}\""), "got: {cmd}");
  }

  #[test]
  fn harness_command_builds_codex_invocation() {
    let cmd = harness_command(
      Harness::Codex,
      Some("gpt-5.5"),
      None,
      "add",
      "tmp/add_codex_result.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(cmd.contains("scsh: harness=codex"));
    // Interactive TUI (no `exec` subcommand) via scsh-tui-record. Folder-trust is seeded
    // host-side into the forwarded config.toml (not in the command), so no in-command seed.
    assert!(cmd.contains("scsh-tui-record 200 50 double-ctrl-c none tmp/add_codex_result.json "), "got: {cmd}");
    assert!(cmd.contains("codex --dangerously-bypass-approvals-and-sandbox"), "got: {cmd}");
    assert!(!cmd.contains("codex exec"), "got: {cmd}");
    assert!(cmd.contains(" -m gpt-5.5"));
    assert!(!cmd.contains("config.toml"), "got: {cmd}");
    assert!(!cmd.contains("capture-pane"), "got: {cmd}");
    assert!(cmd.contains(".skills/add/SKILL.md"));
    assert!(cmd.contains("SCSH_RESULT"));
    assert!(cmd.ends_with("2>&1 | tee \"${SCSH_RUN_LOG}\""));
    let bare = harness_command(
      Harness::Codex,
      None,
      None,
      "multiply",
      "tmp/mul.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(bare.contains("codex --dangerously-bypass-approvals-and-sandbox"));
    assert!(!bare.contains(" -m "));
    assert!(bare.ends_with("2>&1 | tee \"${SCSH_RUN_LOG}\""));
  }

  #[test]
  fn harness_command_builds_grok_invocation() {
    // grok now runs as its recorded interactive Build TUI (positional prompt), not headless `-p`.
    let cmd = harness_command(
      Harness::Grok,
      Some("grok-build"),
      Some("high"),
      "add",
      "tmp/add_grok.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(cmd.contains("scsh: harness=grok"));
    assert!(cmd.contains("scsh-tui-record 200 50 double-ctrl-c none tmp/add_grok.json "), "got: {cmd}");
    assert!(cmd.contains("grok --always-approve"), "got: {cmd}");
    assert!(!cmd.contains("grok -p "), "no headless -p: {cmd}");
    assert!(cmd.contains(" -m grok-build"));
    assert!(cmd.contains(" --effort high"));
    assert!(cmd.contains(".skills/add/SKILL.md"));
    assert!(cmd.contains("SCSH_RESULT"));
    assert!(cmd.ends_with("2>&1 | tee \"${SCSH_RUN_LOG}\""));
    let bare =
      harness_command(Harness::Grok, None, None, "multiply", "tmp/mul.json", crate::config::Terminal::default(), false);
    assert!(bare.contains("grok --always-approve"), "got: {bare}");
    assert!(!bare.contains(" --effort "));
    assert!(!bare.contains(" -m "));
  }

  #[test]
  fn harness_command_builds_cursor_invocation() {
    let cmd = harness_command(
      Harness::Cursor,
      Some("composer-2.5"),
      Some("high"),
      "add",
      "tmp/add_cursor.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(cmd.contains("scsh: harness=cursor"));
    // Interactive TUI via scsh-tui-record. Workspace trust is pre-seeded by creating
    // cursor's marker file in-container (no flag/config key exists), not by scraping.
    assert!(cmd.contains("scsh-tui-record 200 50 double-ctrl-c none tmp/add_cursor.json "), "got: {cmd}");
    assert!(cmd.contains("cursor-agent --force --sandbox disabled"), "got: {cmd}");
    assert!(!cmd.contains("cursor-agent -p"), "got: {cmd}");
    assert!(!cmd.contains("--trust"), "got: {cmd}");
    assert!(cmd.contains(" --model composer-2.5-fast"));
    assert!(cmd.contains(".cursor/projects/home-agent-repo/.workspace-trusted"), "got: {cmd}");
    assert!(!cmd.contains("capture-pane"), "got: {cmd}");
    assert!(!cmd.contains("send-keys"), "got: {cmd}");
    assert!(cmd.contains(".skills/add/SKILL.md"));
    assert!(cmd.ends_with("2>&1 | tee \"${SCSH_RUN_LOG}\""));
    let bare = harness_command(
      Harness::Cursor,
      None,
      None,
      "multiply",
      "tmp/mul.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(bare.contains("cursor-agent --force --sandbox disabled"));
    assert!(!bare.contains(" --model "));
  }

  #[test]
  fn cursor_model_with_effort_maps_to_cursor_agent_slugs() {
    assert_eq!(cursor_model_with_effort("claude-opus-4-8[effort=low]", Some("high")), "claude-opus-4-8[effort=low]");
    assert_eq!(cursor_model_with_effort("composer-2.5", Some("high")), "composer-2.5-fast");
    assert_eq!(cursor_model_with_effort("composer-2.5", Some("low")), "composer-2.5");
    assert_eq!(cursor_model_with_effort("claude-opus-4-8", Some("low")), "claude-opus-4-8-low");
    assert_eq!(cursor_model_with_effort("gpt-5.5", Some("xhigh")), "gpt-5.5-extra-high");
  }

  #[test]
  fn harness_recorded_at_configured_pty_size() {
    // EVERY harness now records via scsh-tui-record with the PTY size as its first two args;
    // the recording path is always ${SCSH_RUN_LOG}.cast.
    let term = crate::config::Terminal { cols: 120, rows: 30 };
    for h in [Harness::Claude, Harness::Codex, Harness::Cursor, Harness::Opencode, Harness::Grok] {
      let cmd = harness_command(h, Some("m"), None, "add", "tmp/add.json", term, false);
      assert!(cmd.contains("scsh-tui-record 120 30 "), "harness {h:?} got: {cmd}");
      assert!(cmd.contains("cast=${SCSH_RUN_LOG}.cast"), "harness {h:?} got: {cmd}");
    }
  }

  #[test]
  fn harness_command_codex_passes_reasoning_effort() {
    let cmd = harness_command(
      Harness::Codex,
      Some("gpt-5.5"),
      Some("xhigh"),
      "add",
      "tmp/add.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(cmd.contains(" -c model_reasoning_effort=xhigh"));
    let without = harness_command(
      Harness::Codex,
      Some("gpt-5.5"),
      None,
      "add",
      "tmp/add.json",
      crate::config::Terminal::default(),
      false,
    );
    assert!(!without.contains("model_reasoning_effort"));
  }

  #[test]
  fn harness_container_env_depends_on_verbosity() {
    assert_eq!(harness_container_env_verbose(Harness::Opencode, true).len(), 1);
    assert_eq!(harness_container_env_verbose(Harness::Opencode, false).len(), 1);
    assert_eq!(harness_container_env_verbose(Harness::Claude, true).len(), 2);
    assert!(harness_container_env_verbose(Harness::Claude, false).is_empty());
    let codex = harness_container_env_verbose(Harness::Codex, true);
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0].0, "RUST_LOG");
    assert!(harness_container_env_verbose(Harness::Codex, false).is_empty());
    let cursor = harness_container_env_verbose(Harness::Cursor, false);
    assert_eq!(cursor.len(), 2);
    assert_eq!(cursor[0].0, "CURSOR_CONFIG_DIR");
    assert_eq!(cursor[1].0, "XDG_CONFIG_HOME");
  }

  #[test]
  fn harness_verbose_disabled_when_scsh_quiet() {
    let key = "SCSH_QUIET";
    let prev = std::env::var_os(key);
    std::env::set_var(key, "1");
    assert!(!harness_verbose_enabled());
    match prev {
      Some(v) => std::env::set_var(key, v),
      None => std::env::remove_var(key),
    }
  }

  #[test]
  fn build_method_depends_on_runtime() {
    assert_eq!(build_method("container"), BuildMethod::ContextDir);
    assert_eq!(build_method("docker"), BuildMethod::Stdin);
    assert_eq!(build_method("podman"), BuildMethod::Stdin);
  }

  #[test]
  fn bake_definition_json_lists_every_target() {
    let df = dockerfile();
    let specs = vec![
      image_build_spec(Harness::Opencode, &df, 501, 20, "UTC"),
      image_build_spec(Harness::Claude, &df, 501, 20, "UTC"),
    ];
    let json = bake_definition_json("/tmp/ctx", &specs, 501, 20, "UTC");
    assert!(json.contains("\"scsh-opencode\""));
    assert!(json.contains("\"scsh-claude\""));
    assert!(json.contains("\"scsh-opencode:latest\""));
    assert!(json.contains("\"scsh-claude:latest\""));
    assert!(json.contains("\"/tmp/ctx\""));
  }

  #[test]
  fn build_command_bake_names_each_target() {
    let cmd = build_command_bake("docker", &["scsh-opencode".into(), "scsh-claude".into()]);
    let want: Vec<String> = vec![
      "docker".into(),
      "buildx".into(),
      "bake".into(),
      "--load".into(),
      "-f".into(),
      "-".into(),
      "scsh-opencode".into(),
      "scsh-claude".into(),
    ];
    assert_eq!(cmd, want);
  }

  #[test]
  fn base_image_fingerprint_matches_scsh_base_target() {
    let df = dockerfile();
    assert_eq!(
      base_image_fingerprint(&df, 501, 20, "UTC"),
      image_build_fingerprint(&df, BASE_IMAGE_TARGET, 501, 20, "UTC")
    );
    assert_ne!(base_image_fingerprint(&df, 501, 20, "UTC"), image_build_fingerprint(&df, "scsh-codex", 501, 20, "UTC"));
  }

  #[test]
  fn image_build_fingerprint_is_stable_and_target_specific() {
    let df = dockerfile();
    let a = image_build_fingerprint(&df, "scsh-opencode", 501, 20, "UTC");
    let b = image_build_fingerprint(&df, "scsh-opencode", 501, 20, "UTC");
    let c = image_build_fingerprint(&df, "scsh-claude", 501, 20, "UTC");
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_eq!(a.len(), 64);
  }

  #[test]
  fn parse_label_from_container_inspect_json() {
    let compact =
      r#"{"variants":[{"config":{"config":{"Labels":{"scsh.generated":"true","scsh.build.fingerprint":"abc123"}}}}}]"#;
    assert_eq!(parse_label_from_container_inspect(compact, BUILD_FINGERPRINT_LABEL).as_deref(), Some("abc123"));
    assert!(parse_label_from_container_inspect(compact, "missing").is_none());
    // Apple Containers pretty-prints with spaces around `:` (session kuovup / add job rebuilt
    // every image because the compact-only needle never matched).
    let pretty = r#"{
  "variants" : [ {
    "config" : {
      "config" : {
        "Labels" : {
          "scsh.build.fingerprint" : "0d358dfff4332f525a8ecfc7606340efd5f0e177a3465dcbf9dbdb8d5e162190",
          "scsh.generated" : "true"
        }
      }
    }
  } ]
}"#;
    assert_eq!(
      parse_label_from_container_inspect(pretty, BUILD_FINGERPRINT_LABEL).as_deref(),
      Some("0d358dfff4332f525a8ecfc7606340efd5f0e177a3465dcbf9dbdb8d5e162190")
    );
  }

  #[test]
  fn image_created_size_parses_inspect_output() {
    let (created, size) = parse_created_size("2026-07-05T01:02:03.123456789Z\t3222111000\n");
    assert_eq!(created.as_deref(), Some("2026-07-05 01:02:03 UTC"));
    assert_eq!(size.as_deref(), Some("3.2GB"));
    // A missing size (or a runtime printing only Created) degrades, never panics.
    let (created, size) = parse_created_size("2026-07-05T01:02:03Z");
    assert_eq!(created.as_deref(), Some("2026-07-05 01:02:03 UTC"));
    assert_eq!(size, None);
    assert_eq!(parse_created_size(""), (None, None));
    // Non-RFC3339 timestamps pass through unchanged.
    assert_eq!(short_created("yesterday"), "yesterday");
    assert_eq!(format_image_size(999), "999B");
    assert_eq!(format_image_size(482_000), "482.0kB");
    assert_eq!(format_image_size(1_500_000), "1.5MB");
  }

  #[test]
  fn commands_have_expected_shape() {
    let fp = image_build_fingerprint("FROM scratch", "scsh-opencode", 1006, 1007, "Europe/Berlin");
    let label = format!("{BUILD_FINGERPRINT_LABEL}={fp}");
    assert_eq!(
      build_command_stdin("docker", "scsh-opencode:latest", "scsh-opencode", 1006, 1007, "Europe/Berlin", &fp, false),
      vec![
        "docker".into(),
        "build".into(),
        "-t".into(),
        "scsh-opencode:latest".into(),
        "--target".into(),
        "scsh-opencode".into(),
        "--build-arg".into(),
        "AGENT_UID=1006".into(),
        "--build-arg".into(),
        "AGENT_GID=1007".into(),
        "--build-arg".into(),
        "TZ=Europe/Berlin".into(),
        "--label".into(),
        label,
        "-".into(),
      ]
    );
    // A forced rebuild inserts --no-cache right after the target, in both build methods.
    let forced = build_command_stdin("docker", "t:l", "t", 1, 1, "UTC", "fp", true);
    assert_eq!(forced[6], "--no-cache");
    let forced_ctx = build_command_context("docker", "t:l", "t", "/ctx", 1, 1, "UTC", "fp", true);
    assert!(forced_ctx.contains(&"--no-cache".to_string()));
    assert!(!build_command_context("docker", "t:l", "t", "/ctx", 1, 1, "UTC", "fp", false)
      .contains(&"--no-cache".to_string()));
    // Host TUI recorder argv (asciinema 3.x) — builds get a real PTY, not --progress=plain.
    let rec = asciinema_rec_argv("/tmp/b.cast", 120, 40, "docker build -t t /ctx");
    assert_eq!(rec[0], "asciinema");
    assert!(rec.contains(&"--headless".to_string()));
    assert!(rec.contains(&"--return".to_string()));
    assert!(rec.contains(&"asciicast-v3".to_string()));
    assert!(rec.contains(&"120x40".to_string()));
    assert!(!build_command_context("container", "t:l", "t", "/ctx", 1, 1, "UTC", "fp", false)
      .contains(&"--progress=plain".to_string()));
    assert_eq!(
      run_command(
        "docker",
        "scsh-opencode:latest",
        "/tmp/run",
        "run-s",
        &[],
        &[],
        "opencode run 'run skill s'",
        RepoMountMode::Full,
      ),
      vec![
        "docker",
        "run",
        "--rm",
        "--name",
        "run-s",
        "-v",
        "/tmp/run:/home/agent/repo",
        "scsh-opencode:latest",
        "/bin/sh",
        "-c",
        "opencode run 'run skill s'"
      ]
    );
    assert_eq!(
      run_command(
        "container",
        "scsh-opencode:latest",
        "/tmp/run",
        "run-s",
        &[],
        &[],
        "git clone",
        RepoMountMode::TmpOnly,
      ),
      vec![
        "container",
        "run",
        "--rm",
        "--name",
        "run-s",
        "-v",
        "/tmp/run/tmp:/home/agent/repo/tmp",
        "scsh-opencode:latest",
        "/bin/sh",
        "-c",
        "git clone"
      ]
    );
    assert_eq!(
      run_command(
        "podman",
        "scsh-claude:latest",
        "/tmp/run",
        "run-s",
        &[],
        &[("/home/u/.claude", "/home/agent/.claude:ro")],
        "claude -p hi",
        RepoMountMode::Full,
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
        "/tmp/run:/home/agent/repo",
        "scsh-claude:latest",
        "/bin/sh",
        "-c",
        "claude -p hi"
      ]
    );
    // opencode uses NO separate mount now — its auth/config is copied into the run clone's tmp/
    // and rides the repo mount, same as the other harnesses.
    assert_eq!(
      run_command(
        "docker",
        "scsh-opencode:latest",
        "/tmp/run",
        "run-s",
        &[],
        &[],
        "opencode run 'run skill s'",
        RepoMountMode::Full,
      ),
      vec![
        "docker",
        "run",
        "--rm",
        "--name",
        "run-s",
        "-v",
        "/tmp/run:/home/agent/repo",
        "scsh-opencode:latest",
        "/bin/sh",
        "-c",
        "opencode run 'run skill s'"
      ]
    );
  }

  #[test]
  fn run_command_forwards_env_as_e_flags() {
    let env = vec![("A".to_string(), "20".to_string()), ("B".to_string(), "22".to_string())];
    assert_eq!(
      run_command(
        "docker",
        "scsh-opencode:latest",
        "/tmp/run",
        "run-s",
        &env,
        &[],
        "opencode run 'run skill s'",
        RepoMountMode::Full,
      ),
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
        "/tmp/run:/home/agent/repo",
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
  fn fsck_command_checks_clone_integrity() {
    assert_eq!(fsck_command("/tmp/dst"), vec!["git", "-C", "/tmp/dst", "fsck", "--no-progress"]);
  }

  #[test]
  fn uses_git_transport_on_macos_apple_container_only() {
    let prev = std::env::var("SCSH_GIT_TRANSPORT").ok();
    std::env::remove_var("SCSH_GIT_TRANSPORT");
    if cfg!(target_os = "macos") {
      assert!(uses_git_transport("container"));
    } else {
      assert!(!uses_git_transport("container"));
    }
    assert!(!uses_git_transport("docker"));
    std::env::set_var("SCSH_GIT_TRANSPORT", "0");
    if cfg!(target_os = "macos") {
      assert!(uses_git_transport("container"), "Apple Container always uses git transport");
    } else {
      assert!(!uses_git_transport("container"));
    }
    std::env::set_var("SCSH_GIT_TRANSPORT", "1");
    assert!(uses_git_transport("docker"));
    match prev {
      Some(v) => std::env::set_var("SCSH_GIT_TRANSPORT", v),
      None => std::env::remove_var("SCSH_GIT_TRANSPORT"),
    }
  }

  #[test]
  fn push_transport_refs_maps_origin_branches_to_heads() {
    use std::process::Command;
    let tmp = std::env::temp_dir().join(format!("scsh-push-transport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let root = tmp.join("root");
    let bare = tmp.join("bare.git");
    Command::new("git").args(["init", "-q"]).arg(&root).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["config", "user.email", "t@example.com"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["config", "user.name", "t"]).status().unwrap();
    std::fs::write(root.join("f"), "x").unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["add", "f"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["commit", "-qm", "init"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["branch", "-M", "main"]).status().unwrap();
    Command::new("git")
      .args(["-C"])
      .arg(&root)
      .args(["remote", "add", "origin", "https://example.invalid/scsh.git"])
      .status()
      .unwrap();
    Command::new("git")
      .args(["-C"])
      .arg(&root)
      .args(["update-ref", "refs/remotes/origin/main", "HEAD"])
      .status()
      .unwrap();
    std::fs::write(root.join("f"), "y").unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["add", "f"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["commit", "-qm", "feature"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["checkout", "-q", "-b", "feature"]).status().unwrap();
    push_transport_refs(&root, &bare).unwrap();
    let show = Command::new("git").args(["-C"]).arg(&bare).args(["show-ref"]).output().unwrap();
    let refs = String::from_utf8_lossy(&show.stdout);
    assert!(refs.contains("refs/heads/main"), "expected refs/heads/main in bare, got:\n{refs}");
    assert!(refs.contains("refs/heads/feature"), "expected feature branch in bare, got:\n{refs}");
    assert!(!refs.contains("refs/remotes/origin/main"), "bare should not store remote-tracking refs:\n{refs}");
    let _ = std::fs::remove_dir_all(&tmp);
  }

  #[test]
  fn push_transport_refs_uses_local_main_not_stale_origin() {
    use std::process::Command;
    let tmp = std::env::temp_dir().join(format!("scsh-push-main-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let root = tmp.join("root");
    let bare = tmp.join("bare.git");
    let work = tmp.join("work");
    Command::new("git").args(["init", "-q"]).arg(&root).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["config", "user.email", "t@example.com"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["config", "user.name", "t"]).status().unwrap();
    std::fs::write(root.join("f"), "stale").unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["add", "f"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["commit", "-qm", "stale"]).status().unwrap();
    let stale = Command::new("git").args(["-C"]).arg(&root).args(["rev-parse", "HEAD"]).output().unwrap();
    let stale = String::from_utf8_lossy(&stale.stdout).trim().to_string();
    std::fs::write(root.join("f"), "base").unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["add", "f"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["commit", "-qm", "base"]).status().unwrap();
    let base_sha = Command::new("git").args(["-C"]).arg(&root).args(["rev-parse", "HEAD"]).output().unwrap();
    let base_sha = String::from_utf8_lossy(&base_sha.stdout).trim().to_string();
    Command::new("git").args(["-C"]).arg(&root).args(["branch", "-M", "main"]).status().unwrap();
    Command::new("git")
      .args(["-C"])
      .arg(&root)
      .args(["remote", "add", "origin", "https://example.invalid/scsh.git"])
      .status()
      .unwrap();
    Command::new("git")
      .args(["-C"])
      .arg(&root)
      .args(["update-ref", "refs/remotes/origin/main", &stale])
      .status()
      .unwrap();
    std::fs::write(root.join("f"), "feature").unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["add", "f"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["commit", "-qm", "feature"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["checkout", "-q", "-b", "feature"]).status().unwrap();
    Command::new("git").args(["-C"]).arg(&root).args(["branch", "-f", "main", &base_sha]).status().unwrap();
    push_transport_refs(&root, &bare).unwrap();
    Command::new("git").args(["clone", "-q"]).arg(&bare).arg(&work).status().unwrap();
    let origin_main = Command::new("git").args(["-C"]).arg(&work).args(["rev-parse", "origin/main"]).output().unwrap();
    let origin_main = String::from_utf8_lossy(&origin_main.stdout).trim().to_string();
    assert_eq!(origin_main, base_sha, "origin/main must match force-updated local main");
    assert_ne!(origin_main, stale, "must not use stale refs/remotes/origin/main");
    let _ = std::fs::remove_dir_all(&tmp);
  }

  #[test]
  fn git_transport_entry_clones_before_harness() {
    let entry = git_transport_entry("echo hi", false, "bot", "bot@example.com");
    assert!(entry.contains("ip -4 route show default"));
    assert!(entry.contains("git clone"));
    assert!(entry.contains("transport.git"));
    assert!(entry.contains(&format!("mkdir -p {AGENT_REPO}/tmp")));
    assert!(entry.contains("origin/main missing after git transport clone"));
    assert!(entry.contains("echo hi"));
    assert!(!entry.contains("pull.git"));
    let entry = git_transport_entry("echo hi", true, "bot", "bot@example.com");
    assert!(entry.contains("pull.git"));
    assert!(entry.contains("user.email"));
  }

  #[test]
  fn opencode_model_provider_is_first_path_segment() {
    assert_eq!(opencode_model_provider("openai/gpt-5.5"), "openai");
    assert_eq!(opencode_model_provider("nebius-glm/zai-org/GLM-5.2"), "nebius-glm");
    assert_eq!(opencode_model_provider("standalone"), "standalone");
  }

  #[test]
  fn opencode_expired_provider_flags_only_expired_oauth() {
    // A fake opencode data home with an EXPIRED openai OAuth login, a valid claude OAuth login,
    // and a never-expiring nebius API key.
    let base = std::env::temp_dir().join(format!("scsh-oc-exp-{}", std::process::id()));
    let auth_dir = base.join("opencode");
    std::fs::create_dir_all(&auth_dir).unwrap();
    std::fs::write(
      auth_dir.join("auth.json"),
      r#"{
        "openai": {"type": "oauth", "access": "x", "expires": 1000000000000},
        "anthropic": {"type": "oauth", "access": "y", "expires": 99999999999999},
        "nebius": {"type": "api", "key": "k"}
      }"#,
    )
    .unwrap();
    let xdg = base.clone().into_os_string();
    let prev = std::env::var_os("XDG_DATA_HOME");
    std::env::set_var("XDG_DATA_HOME", &xdg);
    // openai OAuth is long-expired → flagged with the provider name.
    assert_eq!(opencode_expired_provider("openai/gpt-5.5").as_deref(), Some("openai"));
    // A valid OAuth login is not flagged.
    assert_eq!(opencode_expired_provider("anthropic/claude"), None);
    // A static API key never expires.
    assert_eq!(opencode_expired_provider("nebius/whatever"), None);
    // An unknown provider (no auth entry) is not flagged.
    assert_eq!(opencode_expired_provider("mystery/model"), None);
    match prev {
      Some(v) => std::env::set_var("XDG_DATA_HOME", v),
      None => std::env::remove_var("XDG_DATA_HOME"),
    }
    let _ = std::fs::remove_dir_all(&base);
  }

  #[test]
  fn opencode_providers_for_models_dedupes_and_sorts() {
    let models = std::collections::HashSet::from([
      "openai/gpt-5.5".into(),
      "openai/gpt-5.4-mini-fast".into(),
      "nebius-glm/zai-org/GLM-5.2".into(),
    ]);
    assert_eq!(opencode_providers_for_models(&models), vec!["nebius-glm".to_string(), "openai".to_string()]);
  }

  #[test]
  fn requested_opencode_models_collects_explicit_models_from_selection() {
    let skills = vec![
      crate::config::ResolvedInvocation {
        name: "a".into(),
        skill_source: "add".into(),
        harness: Harness::Opencode,
        model: Some("openai/gpt-5.5".into()),
        effort: None,
        profile: None,
        commits: false,
        timeout: None,
        inactivity_timeout: None,
        env: vec![],
        result: "tmp/a.json".into(),
        terminal: crate::config::Terminal::default(),
        delivery: crate::config::SkillDelivery::Repo,
      },
      crate::config::ResolvedInvocation {
        name: "b".into(),
        skill_source: "add".into(),
        harness: Harness::Claude,
        model: Some("sonnet".into()),
        effort: None,
        profile: None,
        commits: false,
        timeout: None,
        inactivity_timeout: None,
        env: vec![],
        result: "tmp/b.json".into(),
        terminal: crate::config::Terminal::default(),
        delivery: crate::config::SkillDelivery::Repo,
      },
      crate::config::ResolvedInvocation {
        name: "c".into(),
        skill_source: "add".into(),
        harness: Harness::Opencode,
        model: None,
        effort: None,
        profile: None,
        commits: false,
        timeout: None,
        inactivity_timeout: None,
        env: vec![],
        result: "tmp/c.json".into(),
        terminal: crate::config::Terminal::default(),
        delivery: crate::config::SkillDelivery::Repo,
      },
    ];
    let set = requested_opencode_models(&skills);
    assert_eq!(set.len(), 1);
    assert!(set.contains("openai/gpt-5.5"));
  }

  #[test]
  fn parse_opencode_models_collects_trimmed_lines() {
    let set = parse_opencode_models("openai/gpt-5.5\n\nnebius-glm/zai-org/GLM-5.2\n");
    assert_eq!(set.len(), 2);
    assert!(set.contains("openai/gpt-5.5"));
    assert!(set.contains("nebius-glm/zai-org/GLM-5.2"));
  }

  #[test]
  fn opencode_model_probe_checks_listed_models() {
    let probe = OpencodeModelProbe { available: Some(std::collections::HashSet::from(["openai/gpt-5.5".into()])) };
    assert!(probe.check_model("openai/gpt-5.5").is_ok());
    let err = probe.check_model("openai/other").unwrap_err();
    assert!(err.contains("openai/other"));
    assert!(err.contains("opencode models"));
  }

  #[test]
  fn opencode_model_probe_skips_when_not_loaded() {
    let probe = OpencodeModelProbe { available: None };
    assert!(probe.check_model("any/model").is_ok());
  }

  #[test]
  fn opencode_model_probe_rejects_when_model_list_empty() {
    let probe = OpencodeModelProbe { available: Some(std::collections::HashSet::new()) };
    assert!(probe.check_model("openai/anything").is_err());
  }

  #[test]
  fn opencode_model_probe_for_selected_skips_without_explicit_models() {
    let skills = vec![crate::config::ResolvedInvocation {
      name: "add".into(),
      skill_source: "add".into(),
      harness: Harness::Opencode,
      model: None,
      effort: None,
      profile: None,
      commits: false,
      timeout: None,
      inactivity_timeout: None,
      env: vec![],
      result: "tmp/add.json".into(),
      terminal: crate::config::Terminal::default(),
      delivery: crate::config::SkillDelivery::Repo,
    }];
    let probe = OpencodeModelProbe::for_selected(&skills);
    assert!(probe.check_model("openai/anything").is_ok());
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
    let prev_home = std::env::var_os("HOME");
    let empty_home = std::env::temp_dir().join(format!("scsh-empty-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty_home);
    std::fs::create_dir_all(&empty_home).unwrap();
    std::env::remove_var(key);
    std::env::set_var("HOME", &empty_home);
    let err = check_harness_host(Harness::Claude).unwrap_err();
    assert!(err.contains("CLAUDE_CODE_OAUTH_TOKEN"));
    assert!(err.contains("setup-token"));
    match prev {
      Some(v) => std::env::set_var(key, v),
      None => std::env::remove_var(key),
    }
    match prev_home {
      Some(v) => std::env::set_var("HOME", v),
      None => std::env::remove_var("HOME"),
    }
    let _ = std::fs::remove_dir_all(&empty_home);
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
