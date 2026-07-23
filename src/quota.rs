//! Subscription quota probes for the agent harnesses (`scsh quota`).
//!
//! Each supported harness exposes the same usage numbers its own interactive UI shows
//! (claude's `/usage`, codex's `/status`, Cursor's dashboard, grok's billing screen)
//! through an authenticated HTTP endpoint. scsh reads the harness's OWN stored
//! credentials — the same artifacts credential forwarding already locates — and calls
//! that endpoint with `curl`, then normalizes every provider's answer into one shape:
//! percent-used windows with reset times, plus a one-line human summary.
//!
//! Read-only and cost-free: no model calls are made, and no token is ever printed or
//! passed on a command line (curl reads headers from stdin via `--config -`, so secrets
//! never appear in `ps`). Opencode aggregates other providers' subscriptions and has no
//! quota endpoint of its own, so it reports `unsupported` rather than a guess.
//!
//! Structurally, a quota check is a JOB: `quota_cmd` in main.rs registers a session under
//! [`QUOTA_REPO`] with one RUN per harness, and each run writes its own result file
//! ([`result_file_name`], headline via [`result_json`]'s `result` field) and finishes its
//! own proc row — so the session browser shows one status line per harness, exactly like
//! any other fleet. This module only knows how to check one harness and render answers.

use crate::config::Harness;
use crate::json::{self, Value};

/// The harnesses `scsh quota` can actually query, in the canonical UI order.
pub const SUPPORTED: [Harness; 4] = [Harness::Claude, Harness::Codex, Harness::Grok, Harness::Cursor];

/// The synthetic `repo` label quota sessions register under — like `(image builds)`, it
/// never matches a real path, so quota jobs neither trip the one-job-per-repo guard nor
/// get supervisor restarts (a failed check is a fact to show, not a job to redo).
pub const QUOTA_REPO: &str = "(quota)";

/// The profile label quota sessions register under; run names prefix it per harness.
pub const QUOTA_SKILL: &str = "quota";

/// The per-harness run name (`quota-claude`). Every run in a session needs a UNIQUE
/// skill name — the store chains same-kind same-name procs into one retry lineage
/// (`proc_next_attempt`'s legacy fallback), so four runs all named `quota` would render
/// as one task on attempt 4, not four parallel tasks.
pub fn run_name(harness: Harness) -> String {
  format!("{}-{}", QUOTA_SKILL, harness.as_str())
}

/// The per-run result file name for one harness's check.
pub fn result_file_name(harness: Harness) -> String {
  format!("{}.json", run_name(harness))
}

/// One rate-limit window: a stable machine id, a human label, how much is used, and
/// when the window resets (ISO-8601 UTC, `None` when the provider doesn't say).
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
  pub id: String,
  pub label: String,
  pub used_percent: f64,
  pub resets_at: Option<String>,
}

/// The normalized quota answer for one harness. `summary` is always present — for
/// non-`ok` statuses it says what went wrong, so a caller can print it verbatim.
#[derive(Debug, Clone)]
pub struct HarnessQuota {
  pub harness: Harness,
  /// `ok`, `missing` (no credentials), `expired` (credentials lapsed or rejected),
  /// `unsupported` (no quota endpoint for this harness), or `error`.
  pub status: &'static str,
  /// Provider plan slug when known (e.g. `max`, `prolite`, `Ultra`, `GrokPro`).
  pub plan: Option<String>,
  pub windows: Vec<QuotaWindow>,
  /// One human-readable line: `claude (max): 5h session 3% · weekly 57%`.
  pub summary: String,
  /// Actionable next step when not `ok`; empty otherwise.
  pub hint: String,
}

impl HarnessQuota {
  fn ok(harness: Harness, plan: Option<String>, windows: Vec<QuotaWindow>) -> Self {
    let summary = summarize(harness, plan.as_deref(), &windows);
    HarnessQuota { harness, status: "ok", plan, windows, summary, hint: String::new() }
  }

  fn down(harness: Harness, status: &'static str, reason: &str, hint: &str) -> Self {
    HarnessQuota {
      harness,
      status,
      plan: None,
      windows: Vec::new(),
      summary: format!("{}: {reason}", harness.as_str()),
      hint: hint.to_string(),
    }
  }
}

/// `claude (max): 5h session 3% · weekly 57% — first reset 2026-07-23 18:10 UTC`.
fn summarize(harness: Harness, plan: Option<&str>, windows: &[QuotaWindow]) -> String {
  let who = match plan {
    Some(p) => format!("{} ({p})", harness.as_str()),
    None => harness.as_str().to_string(),
  };
  if windows.is_empty() {
    return format!("{who}: no active limit windows reported");
  }
  let gauges: Vec<String> = windows.iter().map(|w| format!("{} {}%", w.label, trim_percent(w.used_percent))).collect();
  let reset = windows.iter().filter_map(|w| w.resets_at.as_deref()).min();
  match reset {
    Some(iso) => format!("{who}: {} — first reset {} UTC", gauges.join(" · "), human_time(iso)),
    None => format!("{who}: {}", gauges.join(" · ")),
  }
}

/// Render a percent without a spurious `.0` (`3` not `3.0`, but `33.6` stays).
fn trim_percent(p: f64) -> String {
  if p.fract() == 0.0 {
    format!("{}", p as i64)
  } else {
    format!("{p:.1}")
  }
}

/// `2026-07-23T18:10:00Z` → `2026-07-23 18:10` (already UTC; seconds dropped).
fn human_time(iso: &str) -> String {
  let t = iso.trim_end_matches('Z');
  match t.split_once('T') {
    Some((d, clock)) => format!("{d} {}", clock.chars().take(5).collect::<String>()),
    None => t.to_string(),
  }
}

/// Normalize provider timestamps to `YYYY-MM-DDTHH:MM:SSZ`: `+00:00` becomes `Z`,
/// fractional seconds are dropped. Non-UTC offsets are left alone (honesty over tidiness).
fn normalize_iso(ts: &str) -> String {
  let ts = ts.trim();
  let ts = ts.strip_suffix("+00:00").map(|t| format!("{t}Z")).unwrap_or_else(|| ts.to_string());
  match (ts.find('.'), ts.ends_with('Z')) {
    (Some(dot), true) => format!("{}Z", &ts[..dot]),
    _ => ts,
  }
}

/// Unix seconds → `YYYY-MM-DDTHH:MM:SSZ`, reusing the run-dir stamp formatter.
fn epoch_to_iso(epoch_secs: u64) -> String {
  let s = crate::runtime::format_utc_timestamp(epoch_secs); // YYYYMMDD-HHMMSS
  format!("{}-{}-{}T{}:{}:{}Z", &s[0..4], &s[4..6], &s[6..8], &s[9..11], &s[11..13], &s[13..15])
}

// ---- tiny Value accessors (the crate has no serde; json::Value is a plain enum) ----

fn get<'a>(obj: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
  obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn get_obj<'a>(obj: &'a [(String, Value)], key: &str) -> Option<&'a [(String, Value)]> {
  match get(obj, key) {
    Some(Value::Object(o)) => Some(o),
    _ => None,
  }
}

fn get_arr<'a>(obj: &'a [(String, Value)], key: &str) -> Option<&'a [Value]> {
  match get(obj, key) {
    Some(Value::Array(a)) => Some(a),
    _ => None,
  }
}

fn get_str<'a>(obj: &'a [(String, Value)], key: &str) -> Option<&'a str> {
  match get(obj, key) {
    Some(Value::String(s)) => Some(s.as_str()),
    _ => None,
  }
}

fn get_num(obj: &[(String, Value)], key: &str) -> Option<f64> {
  match get(obj, key) {
    Some(Value::Number(n)) => Some(*n),
    _ => None,
  }
}

/// Lowercase alphanumerics with `_` separators — window ids from provider labels.
fn slug(s: &str) -> String {
  let mut out = String::new();
  for c in s.chars() {
    if c.is_ascii_alphanumeric() {
      out.push(c.to_ascii_lowercase());
    } else if !out.ends_with('_') && !out.is_empty() {
      out.push('_');
    }
  }
  out.trim_end_matches('_').to_string()
}

// ---- HTTP via curl (no HTTP or TLS dependency in the crate) ----

struct HttpResponse {
  status: u16,
  body: String,
}

/// One HTTPS request through `curl --config -`: the URL and every header (including the
/// Authorization bearer) travel over stdin, never argv. 10s cap, fail-fast.
fn curl(url: &str, headers: &[String], post_body: Option<&str>) -> Result<HttpResponse, String> {
  // Providers throttle bursts on these endpoints (observed live: a 429 right after a
  // sweep). One short in-run retry absorbs the transient case; a persistent 429 is
  // reported honestly by the caller.
  let first = curl_once(url, headers, post_body)?;
  if first.status != 429 {
    return Ok(first);
  }
  std::thread::sleep(std::time::Duration::from_secs(2));
  curl_once(url, headers, post_body)
}

fn curl_once(url: &str, headers: &[String], post_body: Option<&str>) -> Result<HttpResponse, String> {
  use std::io::Write;
  use std::process::{Command, Stdio};
  let mut config = String::new();
  config.push_str(&format!("url = {}\n", curl_config_quote(url)));
  for h in headers {
    config.push_str(&format!("header = {}\n", curl_config_quote(h)));
  }
  if let Some(body) = post_body {
    config.push_str(&format!("data = {}\n", curl_config_quote(body)));
  }
  config.push_str("silent\nshow-error\nmax-time = 10\nwrite-out = \"\\n%{http_code}\"\n");
  let mut child = Command::new("curl")
    .args(["--config", "-"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| format!("could not run curl: {e}"))?;
  child.stdin.as_mut().expect("piped stdin").write_all(config.as_bytes()).map_err(|e| format!("curl stdin: {e}"))?;
  let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
  let text = String::from_utf8_lossy(&out.stdout);
  // The write-out stamp is the final line even on HTTP errors; curl exiting non-zero
  // without it means a transport failure (DNS, timeout, TLS).
  let (body, code) = match text.rsplit_once('\n') {
    Some((b, c)) if c.chars().all(|ch| ch.is_ascii_digit()) && !c.is_empty() => (b, c),
    _ => return Err(format!("curl failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
  };
  let status: u16 = code.parse().map_err(|_| format!("curl wrote a non-numeric status '{code}'"))?;
  if status == 0 {
    return Err(format!("network error: {}", String::from_utf8_lossy(&out.stderr).trim()));
  }
  Ok(HttpResponse { status, body: body.to_string() })
}

/// Quote a value for a curl config file (double quotes, backslash escapes).
fn curl_config_quote(s: &str) -> String {
  format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

// ---- per-provider fetchers (thin: locate credentials, one or two GETs, parse) ----

/// Query one harness's quota. Never panics, never prints; the answer carries its own
/// status and hint. Network only happens when credentials are actually found.
pub fn fetch(harness: Harness) -> HarnessQuota {
  match harness {
    Harness::Claude => claude_quota(),
    Harness::Codex => codex_quota(),
    Harness::Grok => grok_quota(),
    Harness::Cursor => cursor_quota(),
    Harness::Opencode => HarnessQuota::down(
      harness,
      "unsupported",
      "no quota endpoint (opencode fronts other providers' subscriptions)",
      "check the underlying provider's own dashboard instead",
    ),
  }
}

fn expired_hint(cli: &str) -> String {
  format!("run any `{cli}` command on the host to refresh the token, then retry")
}

/// A 429 that survived the in-run retry: the provider is throttling, not broken.
fn rate_limited(h: Harness, what: &str) -> HarnessQuota {
  HarnessQuota::down(
    h,
    "error",
    &format!("{what} rate-limited the check (HTTP 429)"),
    "wait a minute and re-run — the endpoint throttles bursts",
  )
}

fn claude_quota() -> HarnessQuota {
  let h = Harness::Claude;
  // The same precedence credential forwarding uses: env token, credentials file, keychain.
  // The file/keychain blob also carries the plan (`subscriptionType`) and expiry.
  let (token, plan) = match claude_token_and_plan() {
    Ok(pair) => pair,
    Err(quota) => return quota,
  };
  let resp = match curl("https://api.anthropic.com/api/oauth/usage", &[format!("Authorization: Bearer {token}")], None)
  {
    Ok(r) => r,
    Err(e) => return HarnessQuota::down(h, "error", &e, ""),
  };
  match resp.status {
    200 => match parse_claude_usage(&resp.body) {
      Ok(windows) => HarnessQuota::ok(h, plan, windows),
      Err(e) => HarnessQuota::down(h, "error", &e, ""),
    },
    // The harness's own CLI is the fallback for both: it talks to the same backend
    // through its own client — refreshing a stale token itself, and riding out endpoint
    // throttling the raw call cannot (observed live: direct 429 while the CLI answers).
    // A failed fallback names its reason in the report — a silent one is undebuggable.
    401 | 403 => claude_cli_usage(plan.clone()).unwrap_or_else(|why| {
      HarnessQuota::down(
        h,
        "expired",
        &format!("the stored OAuth token was rejected, and the CLI fallback failed ({why})"),
        &expired_hint("claude"),
      )
    }),
    429 => claude_cli_usage(plan.clone()).unwrap_or_else(|why| {
      HarnessQuota::down(
        h,
        "error",
        &format!("usage endpoint rate-limited the check (HTTP 429), and the CLI fallback failed ({why})"),
        "wait a minute and re-run — the endpoint throttles bursts",
      )
    }),
    code => HarnessQuota::down(h, "error", &format!("usage endpoint answered HTTP {code}"), ""),
  }
}

/// Headless fallback through the harness's own tool: `claude /usage --print` is the
/// interactive `/usage` screen without the TUI — no model call, no tokens, ~1s. Its
/// answer is prose with LOCAL-timezone reset times ("resets Jul 26 at 5pm (Europe/
/// London)"), which cannot be normalized to UTC without a tz database, so fallback
/// windows carry no `resets_at` — honestly degraded rather than wrongly converted.
fn claude_cli_usage(plan: Option<String>) -> Result<HarnessQuota, String> {
  let Some(bin) = crate::runtime::which("claude") else {
    return Err("claude is not on PATH".into());
  };
  let mut cmd = std::process::Command::new(bin);
  cmd.args(["/usage", "--print", "--output-format", "json", "--no-session-persistence"]);
  // A neutral cwd: the CLI is project-aware, and quota must not depend on (or touch)
  // whatever repository the scsh run happens to sit in.
  cmd.current_dir(std::env::temp_dir());
  // Pin the CLI to its stored claude.ai OAuth. Subscription quota is this check's whole
  // contract, and any of these in the calling shell flips the CLI into API-key/gateway
  // mode, where `/usage` has no gauges and prints a cost report instead (observed live).
  for var in [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
  ] {
    cmd.env_remove(var);
  }
  let out = cmd.stdin(std::process::Stdio::null()).output().map_err(|e| format!("could not run claude: {e}"))?;
  if !out.status.success() {
    let why = glimpse(&out.stderr).or_else(|| glimpse(&out.stdout)).unwrap_or_else(|| "no output".into());
    let code = out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
    return Err(format!("claude exited {code}: {why}"));
  }
  let text = String::from_utf8_lossy(&out.stdout);
  let Some(result) = json::field(&text, "result") else {
    return Err(format!("claude /usage printed no result field: {}", glimpse(&out.stdout).unwrap_or_default()));
  };
  let windows = parse_claude_usage_text(&result);
  if windows.is_empty() {
    return Err(format!("claude /usage reported no gauges: {}", glimpse(result.as_bytes()).unwrap_or_default()));
  }
  Ok(HarnessQuota::ok(Harness::Claude, plan, windows))
}

/// First non-empty line of a child's output, capped for a one-line diagnostic.
fn glimpse(bytes: &[u8]) -> Option<String> {
  let text = String::from_utf8_lossy(bytes);
  let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
  Some(line.chars().take(160).collect())
}

/// Parse the `/usage` prose: every `Current <window>: <n>% used …` line is a gauge
/// (`session`, `week (all models)`, `week (<model family>)`); the local-behavior
/// breakdown below those lines never matches the prefix and is skipped.
fn parse_claude_usage_text(text: &str) -> Vec<QuotaWindow> {
  let mut out = Vec::new();
  for line in text.lines() {
    let Some(rest) = line.trim().strip_prefix("Current ") else { continue };
    let Some((head, tail)) = rest.split_once(':') else { continue };
    let Some(pct) = tail.split('%').next().and_then(|s| s.trim().parse::<f64>().ok()) else { continue };
    let (id, label) = match head.trim() {
      "session" => ("session_5h".to_string(), "5h session".to_string()),
      "week (all models)" => ("weekly".to_string(), "weekly".to_string()),
      other => {
        let scope = other.strip_prefix("week (").and_then(|s| s.strip_suffix(')')).unwrap_or(other);
        (format!("weekly_{}", slug(scope)), format!("weekly ({scope})"))
      }
    };
    out.push(QuotaWindow { id, label, used_percent: pct, resets_at: None });
  }
  out
}

fn claude_token_and_plan() -> Result<(String, Option<String>), HarnessQuota> {
  let h = Harness::Claude;
  if let Some(token) = crate::runtime::claude_oauth_token() {
    return Ok((token, None));
  }
  let blob = claude_credentials_blob().ok_or_else(|| {
    HarnessQuota::down(
      h,
      "missing",
      "no claude.ai login found",
      "log in with `claude` on the host (subscription quota needs claude.ai OAuth, not an API key)",
    )
  })?;
  match parse_claude_credentials(&blob) {
    Ok(pair) => Ok(pair),
    Err(e) => Err(HarnessQuota::down(h, "error", &e, "")),
  }
}

/// The raw credentials JSON: `~/.claude/.credentials.json` or the macOS keychain item.
fn claude_credentials_blob() -> Option<String> {
  if let Some(home) = std::env::var_os("HOME") {
    let path = std::path::PathBuf::from(home).join(".claude").join(".credentials.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
      if text.contains("claudeAiOauth") {
        return Some(text);
      }
    }
  }
  crate::runtime::claude_keychain_credentials_json()
}

/// Pull `accessToken` + `subscriptionType` out of Claude Code's credentials JSON.
fn parse_claude_credentials(blob: &str) -> Result<(String, Option<String>), String> {
  let Ok(Value::Object(root)) = json::parse(blob) else {
    return Err("could not parse the stored claude credentials".into());
  };
  let oauth = get_obj(&root, "claudeAiOauth").ok_or("stored claude credentials have no claudeAiOauth object")?;
  let token = get_str(oauth, "accessToken").ok_or("stored claude credentials have no access token")?;
  Ok((token.to_string(), get_str(oauth, "subscriptionType").map(str::to_string)))
}

/// Normalize `GET api.anthropic.com/api/oauth/usage`. The `limits` array is the
/// provider's own normalized view — one entry per gauge — so parse that; the legacy
/// `five_hour`/`seven_day` pair is the fallback for older shapes.
fn parse_claude_usage(body: &str) -> Result<Vec<QuotaWindow>, String> {
  let Ok(Value::Object(root)) = json::parse(body) else {
    return Err("usage endpoint answered non-JSON".into());
  };
  let mut windows = Vec::new();
  if let Some(limits) = get_arr(&root, "limits") {
    for item in limits {
      let Value::Object(lim) = item else { continue };
      let Some(percent) = get_num(lim, "percent") else { continue };
      let resets_at = get_str(lim, "resets_at").map(normalize_iso);
      let (id, label) = match get_str(lim, "kind").unwrap_or("") {
        "session" => ("session_5h".to_string(), "5h session".to_string()),
        "weekly_all" => ("weekly".to_string(), "weekly".to_string()),
        kind => {
          // Scoped gauges carry the model family they cap (e.g. "Fable" — the Opus-class
          // weekly cap); fall back to the raw kind for shapes we don't know yet.
          let scope_name = get_obj(lim, "scope")
            .and_then(|s| get_obj(s, "model"))
            .and_then(|m| get_str(m, "display_name"))
            .unwrap_or(kind);
          (format!("weekly_{}", slug(scope_name)), format!("weekly ({scope_name})"))
        }
      };
      windows.push(QuotaWindow { id, label, used_percent: percent, resets_at });
    }
  }
  if windows.is_empty() {
    for (key, id, label) in [("five_hour", "session_5h", "5h session"), ("seven_day", "weekly", "weekly")] {
      if let Some(w) = get_obj(&root, key) {
        if let Some(pct) = get_num(w, "utilization") {
          windows.push(QuotaWindow {
            id: id.to_string(),
            label: label.to_string(),
            used_percent: pct,
            resets_at: get_str(w, "resets_at").map(normalize_iso),
          });
        }
      }
    }
  }
  if windows.is_empty() {
    return Err("usage endpoint answered without any limit gauges".into());
  }
  Ok(windows)
}

fn codex_quota() -> HarnessQuota {
  let h = Harness::Codex;
  let Some(path) = crate::runtime::codex_auth_file_on_host() else {
    return HarnessQuota::down(
      h,
      "missing",
      "no ChatGPT login found",
      "run `codex login` on the host (subscription quota needs a ChatGPT account, not an API key)",
    );
  };
  let Ok(text) = std::fs::read_to_string(&path) else {
    return HarnessQuota::down(h, "error", "could not read ~/.codex/auth.json", "");
  };
  let (token, account) = match parse_codex_auth(&text) {
    Ok(pair) => pair,
    Err(e) => return HarnessQuota::down(h, "error", &e, ""),
  };
  let mut headers = vec![format!("Authorization: Bearer {token}")];
  if let Some(acc) = account {
    headers.push(format!("chatgpt-account-id: {acc}"));
  }
  let resp = match curl("https://chatgpt.com/backend-api/wham/usage", &headers, None) {
    Ok(r) => r,
    Err(e) => return HarnessQuota::down(h, "error", &e, ""),
  };
  match resp.status {
    200 => match parse_codex_usage(&resp.body) {
      Ok((plan, windows)) => HarnessQuota::ok(h, plan, windows),
      Err(e) => HarnessQuota::down(h, "error", &e, ""),
    },
    401 | 403 => HarnessQuota::down(h, "expired", "the stored ChatGPT token was rejected", &expired_hint("codex")),
    429 => rate_limited(h, "usage endpoint"),
    code => HarnessQuota::down(h, "error", &format!("usage endpoint answered HTTP {code}"), ""),
  }
}

/// `~/.codex/auth.json` → (access token, account id). API-key-only files have no tokens.
fn parse_codex_auth(text: &str) -> Result<(String, Option<String>), String> {
  let Ok(Value::Object(root)) = json::parse(text) else {
    return Err("could not parse ~/.codex/auth.json".into());
  };
  let tokens = get_obj(&root, "tokens")
    .ok_or("~/.codex/auth.json has no ChatGPT tokens (API-key auth has no subscription quota)")?;
  let token = get_str(tokens, "access_token").ok_or("~/.codex/auth.json has no access_token")?;
  Ok((token.to_string(), get_str(tokens, "account_id").map(str::to_string)))
}

/// One codex rate-limit window; the window length tells us which gauge it is.
fn codex_window(w: &[(String, Value)]) -> Option<QuotaWindow> {
  let percent = get_num(w, "used_percent")?;
  let resets_at = get_num(w, "reset_at").map(|e| epoch_to_iso(e as u64));
  let (id, label) = match get_num(w, "limit_window_seconds").map(|s| s as u64) {
    Some(18_000) => ("session_5h".to_string(), "5h session".to_string()),
    Some(604_800) => ("weekly".to_string(), "weekly".to_string()),
    Some(secs) => (format!("window_{}h", secs / 3600), format!("{}h window", secs / 3600)),
    None => ("window".to_string(), "window".to_string()),
  };
  Some(QuotaWindow { id, label, used_percent: percent, resets_at })
}

/// Normalize `GET chatgpt.com/backend-api/wham/usage`. Only one of primary/secondary may
/// be populated, so gauges are keyed on `limit_window_seconds`, never on position.
fn parse_codex_usage(body: &str) -> Result<(Option<String>, Vec<QuotaWindow>), String> {
  let Ok(Value::Object(root)) = json::parse(body) else {
    return Err("usage endpoint answered non-JSON".into());
  };
  let plan = get_str(&root, "plan_type").map(str::to_string);
  let mut windows = Vec::new();
  if let Some(rl) = get_obj(&root, "rate_limit") {
    for key in ["primary_window", "secondary_window"] {
      if let Some(w) = get_obj(rl, key) {
        windows.extend(codex_window(w));
      }
    }
  }
  // Per-feature meters (e.g. a per-model weekly cap) ride along with their own names.
  if let Some(extra) = get_arr(&root, "additional_rate_limits") {
    for item in extra {
      let Value::Object(entry) = item else { continue };
      let Some(name) = get_str(entry, "limit_name") else { continue };
      let Some(rl) = get_obj(entry, "rate_limit") else { continue };
      for key in ["primary_window", "secondary_window"] {
        if let Some(w) = get_obj(rl, key) {
          if let Some(mut win) = codex_window(w) {
            win.id = format!("{}_{}", win.id, slug(name));
            win.label = format!("{} ({name})", win.label);
            windows.push(win);
          }
        }
      }
    }
  }
  if windows.is_empty() {
    return Err("usage endpoint answered without any rate-limit windows".into());
  }
  Ok((plan, windows))
}

fn grok_quota() -> HarnessQuota {
  let h = Harness::Grok;
  let Some(path) = crate::runtime::grok_auth_file_on_host() else {
    return HarnessQuota::down(
      h,
      "missing",
      "no grok.com login found",
      "run `grok login` on the host (billing data needs grok.com auth, not an API key)",
    );
  };
  // The grok session token only lives ~6 hours; the file records its expiry, so a lapsed
  // login is reported as such without a doomed network call.
  if crate::runtime::grok_auth_expired() {
    return HarnessQuota::down(h, "expired", "the stored grok session has lapsed", &expired_hint("grok"));
  }
  let Ok(text) = std::fs::read_to_string(&path) else {
    return HarnessQuota::down(h, "error", "could not read the grok auth file", "");
  };
  let token = match parse_grok_auth(&text) {
    Ok(t) => t,
    Err(e) => return HarnessQuota::down(h, "error", &e, ""),
  };
  let base =
    std::env::var("GROK_CLI_CHAT_PROXY_BASE_URL").unwrap_or_else(|_| "https://cli-chat-proxy.grok.com/v1".into());
  let headers = [format!("Authorization: Bearer {token}")];
  let resp = match curl(&format!("{base}/billing?format=credits"), &headers, None) {
    Ok(r) => r,
    Err(e) => return HarnessQuota::down(h, "error", &e, ""),
  };
  match resp.status {
    200 => match parse_grok_billing(&resp.body) {
      Ok(windows) => {
        // Plan name lives on a second endpoint; quota stays useful without it.
        let plan = curl(&format!("{base}/user?include=subscription"), &headers, None)
          .ok()
          .filter(|r| r.status == 200)
          .and_then(|r| parse_grok_subscription(&r.body));
        HarnessQuota::ok(h, plan, windows)
      }
      Err(e) => HarnessQuota::down(h, "error", &e, ""),
    },
    401 | 403 => HarnessQuota::down(h, "expired", "the stored grok session was rejected", &expired_hint("grok")),
    429 => rate_limited(h, "billing endpoint"),
    code => HarnessQuota::down(h, "error", &format!("billing endpoint answered HTTP {code}"), ""),
  }
}

/// `~/.grok/auth.json` is keyed by auth-provider URL; each value carries the session `key`.
fn parse_grok_auth(text: &str) -> Result<String, String> {
  let Ok(Value::Object(root)) = json::parse(text) else {
    return Err("could not parse the grok auth file".into());
  };
  for (_, v) in &root {
    if let Value::Object(entry) = v {
      if let Some(key) = get_str(entry, "key") {
        return Ok(key.to_string());
      }
    }
  }
  Err("the grok auth file has no session key".into())
}

/// Normalize `GET cli-chat-proxy.grok.com/v1/billing?format=credits`: the overall credit
/// gauge plus one gauge per product, all resetting at the current period's end.
fn parse_grok_billing(body: &str) -> Result<Vec<QuotaWindow>, String> {
  let Ok(Value::Object(root)) = json::parse(body) else {
    return Err("billing endpoint answered non-JSON".into());
  };
  let config = get_obj(&root, "config").ok_or("billing endpoint answered without a config object")?;
  let period_end = get_obj(config, "currentPeriod").and_then(|p| get_str(p, "end")).map(normalize_iso);
  let mut windows = Vec::new();
  if let Some(pct) = get_num(config, "creditUsagePercent") {
    windows.push(QuotaWindow {
      id: "weekly".into(),
      label: "weekly credits".into(),
      used_percent: pct,
      resets_at: period_end.clone(),
    });
  }
  if let Some(products) = get_arr(config, "productUsage") {
    for item in products {
      let Value::Object(p) = item else { continue };
      let Some(name) = get_str(p, "product") else { continue };
      // A product entry without a percent means untouched this period, not unknown.
      let pct = get_num(p, "usagePercent").unwrap_or(0.0);
      windows.push(QuotaWindow {
        id: format!("weekly_{}", slug(name)),
        label: format!("weekly ({name})"),
        used_percent: pct,
        resets_at: period_end.clone(),
      });
    }
  }
  if windows.is_empty() {
    return Err("billing endpoint answered without any usage gauges".into());
  }
  Ok(windows)
}

fn parse_grok_subscription(body: &str) -> Option<String> {
  let Ok(Value::Object(root)) = json::parse(body) else { return None };
  get_str(&root, "subscriptionTier").map(str::to_string)
}

fn cursor_quota() -> HarnessQuota {
  let h = Harness::Cursor;
  let Some(token) = cursor_token() else {
    return HarnessQuota::down(h, "missing", "no Cursor login found", "run `cursor-agent login` on the host");
  };
  let headers = [
    format!("Authorization: Bearer {token}"),
    "Content-Type: application/json".to_string(),
    "connect-protocol-version: 1".to_string(),
  ];
  let rpc = "https://api2.cursor.sh/aiserver.v1.DashboardService";
  let resp = match curl(&format!("{rpc}/GetCurrentPeriodUsage"), &headers, Some("{}")) {
    Ok(r) => r,
    Err(e) => return HarnessQuota::down(h, "error", &e, ""),
  };
  match resp.status {
    200 => match parse_cursor_usage(&resp.body) {
      Ok(windows) => {
        let plan = curl(&format!("{rpc}/GetPlanInfo"), &headers, Some("{}"))
          .ok()
          .filter(|r| r.status == 200)
          .and_then(|r| parse_cursor_plan(&r.body));
        HarnessQuota::ok(h, plan, windows)
      }
      Err(e) => HarnessQuota::down(h, "error", &e, ""),
    },
    401 | 403 => {
      HarnessQuota::down(h, "expired", "the stored Cursor token was rejected", &expired_hint("cursor-agent"))
    }
    429 => rate_limited(h, "usage endpoint"),
    code => HarnessQuota::down(h, "error", &format!("usage endpoint answered HTTP {code}"), ""),
  }
}

/// Keychain first (macOS), then auth.json, then the API-key env — same order forwarding uses.
fn cursor_token() -> Option<String> {
  if let Some(t) = crate::runtime::cursor_keychain_access_token() {
    return Some(t);
  }
  if let Some(path) = crate::runtime::cursor_auth_file_on_host() {
    if let Ok(text) = std::fs::read_to_string(&path) {
      if let Ok(Value::Object(root)) = json::parse(&text) {
        if let Some(t) = get_str(&root, "accessToken").or_else(|| get_str(&root, "apiKey")) {
          return Some(t.to_string());
        }
      }
    }
  }
  crate::runtime::cursor_api_key()
}

/// Normalize `DashboardService/GetCurrentPeriodUsage`: the included-usage gauge plus the
/// auto/named-model pool split, all resetting when the billing cycle rolls over.
fn parse_cursor_usage(body: &str) -> Result<Vec<QuotaWindow>, String> {
  let Ok(Value::Object(root)) = json::parse(body) else {
    return Err("usage endpoint answered non-JSON".into());
  };
  let cycle_end = cursor_epoch_ms(&root, "billingCycleEnd").map(epoch_to_iso);
  let plan_usage = get_obj(&root, "planUsage").ok_or("usage endpoint answered without a planUsage object")?;
  let gauges = [
    ("totalPercentUsed", "billing_cycle", "billing cycle (included)"),
    ("autoPercentUsed", "auto_pool", "auto-model pool"),
    ("apiPercentUsed", "api_pool", "named-model pool"),
  ];
  let mut windows = Vec::new();
  for (key, id, label) in gauges {
    if let Some(pct) = get_num(plan_usage, key) {
      windows.push(QuotaWindow {
        id: id.to_string(),
        label: label.to_string(),
        used_percent: pct,
        resets_at: cycle_end.clone(),
      });
    }
  }
  if windows.is_empty() {
    return Err("usage endpoint answered without any percent gauges".into());
  }
  Ok(windows)
}

/// Cursor sends epoch-milliseconds as JSON strings (`"1786460593000"`) or numbers.
fn cursor_epoch_ms(obj: &[(String, Value)], key: &str) -> Option<u64> {
  let ms = match get(obj, key)? {
    Value::String(s) => s.parse::<u64>().ok()?,
    Value::Number(n) => *n as u64,
    _ => return None,
  };
  Some(ms / 1000)
}

fn parse_cursor_plan(body: &str) -> Option<String> {
  let Ok(Value::Object(root)) = json::parse(body) else { return None };
  get_obj(&root, "planInfo").and_then(|p| get_str(p, "planName")).map(str::to_string)
}

// ---- rendering ----

/// The machine-readable answer: one object per harness, plus ok/total counters.
/// Same shape for `scsh quota --json` and the daemon's Setup quota endpoint.
/// One harness's quota as a JSON object body (shared by the aggregate document and the
/// per-run result file): `"harness"`, `"status"`, `"plan"`, `"windows"`, `"summary"`, `"hint"`.
fn harness_json_fields(q: &HarnessQuota) -> String {
  let windows: Vec<String> = q
    .windows
    .iter()
    .map(|w| {
      format!(
        "{{ \"id\": {}, \"label\": {}, \"used_percent\": {}, \"resets_at\": {} }}",
        json::quote(&w.id),
        json::quote(&w.label),
        w.used_percent,
        w.resets_at.as_deref().map(json::quote).unwrap_or_else(|| "null".into()),
      )
    })
    .collect();
  format!(
    "\"harness\": {}, \"status\": {}, \"plan\": {}, \"windows\": [{}], \"summary\": {}, \"hint\": {}",
    json::quote(q.harness.as_str()),
    json::quote(q.status),
    q.plan.as_deref().map(json::quote).unwrap_or_else(|| "null".into()),
    windows.join(", "),
    json::quote(&q.summary),
    json::quote(&q.hint),
  )
}

/// The per-RUN result file for one harness's check. The leading `result` field is the
/// human status line — `json::message` picks it up, so the job page's headline for this
/// run reads like `claude (max): 5h session 3% · weekly 57%` instead of a file path.
pub fn result_json(q: &HarnessQuota) -> String {
  format!("{{ \"result\": {}, {} }}", json::quote(&q.summary), harness_json_fields(q))
}

pub fn render_json(rows: &[HarnessQuota], checked_at: u64) -> String {
  let ok = rows.iter().filter(|q| q.status == "ok").count();
  let harnesses: Vec<String> = rows.iter().map(|q| format!("    {{ {} }}", harness_json_fields(q))).collect();
  format!(
    "{{\n  \"harnesses\": [\n{}\n  ],\n  \"ok\": {ok},\n  \"total\": {},\n  \"checked_at\": {checked_at}\n}}",
    harnesses.join(",\n"),
    rows.len(),
  )
}

/// The human table: one line per window, harness/plan shown on the first line of each
/// group, `used` right-aligned, resets in UTC minutes.
pub fn render_table(rows: &[HarnessQuota]) -> String {
  let mut lines: Vec<[String; 5]> = Vec::new();
  for q in rows {
    let plan = q.plan.clone().unwrap_or_else(|| "-".into());
    if q.windows.is_empty() {
      lines.push([q.harness.as_str().into(), plan, format!("({})", q.status), "-".into(), "-".into()]);
      continue;
    }
    for (i, w) in q.windows.iter().enumerate() {
      let (name, plan) = if i == 0 { (q.harness.as_str().to_string(), plan.clone()) } else { Default::default() };
      lines.push([
        name,
        plan,
        w.label.clone(),
        format!("{}%", trim_percent(w.used_percent)),
        w.resets_at.as_deref().map(human_time).unwrap_or_else(|| "-".into()),
      ]);
    }
  }
  let header = ["harness", "plan", "window", "used", "resets (UTC)"];
  let mut widths = header.map(str::len);
  for row in &lines {
    for (i, cell) in row.iter().enumerate() {
      widths[i] = widths[i].max(cell.len());
    }
  }
  let render = |cells: [&str; 5]| -> String {
    let mut out = String::new();
    for (i, cell) in cells.iter().enumerate() {
      if i > 0 {
        out.push_str("  ");
      }
      if i == 3 {
        out.push_str(&format!("{cell:>w$}", w = widths[i])); // `used` right-aligns
      } else {
        out.push_str(&format!("{cell:<w$}", w = widths[i]));
      }
    }
    out.trim_end().to_string()
  };
  let mut out = vec![render(header)];
  for row in &lines {
    out.push(render([&row[0], &row[1], &row[2], &row[3], &row[4]]));
  }
  out.join("\n")
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Trimmed live response from `api.anthropic.com/api/oauth/usage` (values anonymized).
  const CLAUDE_USAGE: &str = r#"{
    "five_hour": {"utilization": 3.0, "resets_at": "2026-07-23T18:10:00+00:00"},
    "seven_day": {"utilization": 57.0, "resets_at": "2026-07-26T16:00:00+00:00"},
    "limits": [
      {"kind": "session", "group": "session", "percent": 3, "severity": "normal",
       "resets_at": "2026-07-23T18:10:00+00:00", "scope": null, "is_active": false},
      {"kind": "weekly_all", "group": "weekly", "percent": 57, "severity": "normal",
       "resets_at": "2026-07-26T16:00:00+00:00", "scope": null, "is_active": true},
      {"kind": "weekly_scoped", "group": "weekly", "percent": 21, "severity": "normal",
       "resets_at": "2026-07-26T16:00:00+00:00",
       "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}, "is_active": false}
    ],
    "extra_usage": {"is_enabled": false},
    "spend": {"percent": 0, "severity": "normal", "enabled": false}
  }"#;

  #[test]
  fn claude_usage_prefers_the_limits_array() {
    let windows = parse_claude_usage(CLAUDE_USAGE).unwrap();
    assert_eq!(windows.len(), 3);
    assert_eq!(
      windows[0],
      QuotaWindow {
        id: "session_5h".into(),
        label: "5h session".into(),
        used_percent: 3.0,
        resets_at: Some("2026-07-23T18:10:00Z".into()),
      }
    );
    assert_eq!(windows[1].id, "weekly");
    assert_eq!(windows[2].id, "weekly_fable");
    assert_eq!(windows[2].label, "weekly (Fable)");
    assert_eq!(windows[2].used_percent, 21.0);
  }

  #[test]
  fn claude_usage_falls_back_to_the_legacy_pair() {
    let legacy = r#"{
      "five_hour": {"utilization": 4.5, "resets_at": "2026-07-23T18:10:00+00:00"},
      "seven_day": {"utilization": 57.0, "resets_at": "2026-07-26T16:00:00+00:00"}
    }"#;
    let windows = parse_claude_usage(legacy).unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].used_percent, 4.5);
    assert_eq!(windows[1].resets_at.as_deref(), Some("2026-07-26T16:00:00Z"));
    assert!(parse_claude_usage("{}").is_err());
    assert!(parse_claude_usage("not json").is_err());
  }

  /// Live-captured `claude /usage --print` prose (values anonymized) — the 429/expired
  /// fallback path. Reset times are local-timezone prose, so windows carry none.
  #[test]
  fn claude_usage_prose_parses_the_gauges_and_skips_the_breakdown() {
    let text = "You are currently using your subscription to power your Claude Code usage\n\n\
      Current session: 22% used · resets Jul 23 at 7:10pm (Europe/London)\n\
      Current week (all models): 60% used · resets Jul 26 at 5pm (Europe/London)\n\
      Current week (Fable): 21% used · resets Jul 26 at 4:59pm (Europe/London)\n\n\
      What's contributing to your limits usage?\n\
      Last 24h · 2345 requests · 30 sessions\n\
      81% of your usage was at >150k context\n";
    let windows = parse_claude_usage_text(text);
    assert_eq!(windows.len(), 3);
    assert_eq!(
      windows[0],
      QuotaWindow { id: "session_5h".into(), label: "5h session".into(), used_percent: 22.0, resets_at: None }
    );
    assert_eq!(windows[1].id, "weekly");
    assert_eq!(windows[1].used_percent, 60.0);
    assert_eq!(windows[2].id, "weekly_fable");
    assert_eq!(windows[2].label, "weekly (Fable)");
    // Prose without gauges (logged-out or reworded output) parses to nothing — the
    // fallback then reports the original endpoint failure instead of a fabricated ok.
    assert!(parse_claude_usage_text("You are not logged in.\n").is_empty());
  }

  #[test]
  fn claude_credentials_yield_token_and_plan() {
    let blob = r#"{"claudeAiOauth": {"accessToken": "tok-123", "refreshToken": "r",
      "expiresAt": 1785258441000, "scopes": ["user:inference"], "subscriptionType": "max"}}"#;
    let (token, plan) = parse_claude_credentials(blob).unwrap();
    assert_eq!(token, "tok-123");
    assert_eq!(plan.as_deref(), Some("max"));
    assert!(parse_claude_credentials("{}").is_err());
  }

  /// Trimmed live response from `chatgpt.com/backend-api/wham/usage`.
  const CODEX_USAGE: &str = r#"{
    "plan_type": "prolite",
    "rate_limit": {
      "allowed": true, "limit_reached": false,
      "primary_window": {"used_percent": 2, "limit_window_seconds": 604800,
                         "reset_after_seconds": 443828, "reset_at": 1785258441},
      "secondary_window": null
    },
    "additional_rate_limits": [
      {"limit_name": "GPT-5.3-Codex-Spark", "metered_feature": "codex_bengalfox",
       "rate_limit": {"primary_window": {"used_percent": 0, "limit_window_seconds": 604800,
                                          "reset_at": 1785258441}}}
    ],
    "credits": {"has_credits": false, "unlimited": false, "balance": "0"}
  }"#;

  #[test]
  fn codex_usage_keys_windows_on_length_not_position() {
    let (plan, windows) = parse_codex_usage(CODEX_USAGE).unwrap();
    assert_eq!(plan.as_deref(), Some("prolite"));
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].id, "weekly");
    assert_eq!(windows[0].used_percent, 2.0);
    assert_eq!(windows[0].resets_at.as_deref(), Some("2026-07-28T17:07:21Z"));
    assert_eq!(windows[1].id, "weekly_gpt_5_3_codex_spark");
    assert_eq!(windows[1].label, "weekly (GPT-5.3-Codex-Spark)");
  }

  #[test]
  fn codex_5h_window_is_recognized_by_its_length() {
    let body = r#"{"rate_limit": {"primary_window": {"used_percent": 88.5, "limit_window_seconds": 18000}}}"#;
    let (plan, windows) = parse_codex_usage(body).unwrap();
    assert_eq!(plan, None);
    assert_eq!(windows[0].id, "session_5h");
    assert_eq!(windows[0].label, "5h session");
    assert_eq!(windows[0].resets_at, None);
    assert!(parse_codex_usage(r#"{"rate_limit": {}}"#).is_err());
  }

  #[test]
  fn codex_auth_needs_chatgpt_tokens() {
    let auth = r#"{"auth_mode": "chatgpt", "OPENAI_API_KEY": null,
      "tokens": {"id_token": "i", "access_token": "at-9", "refresh_token": "r", "account_id": "acc-7"},
      "last_refresh": "2026-07-23T10:00:00Z"}"#;
    assert_eq!(parse_codex_auth(auth).unwrap(), ("at-9".to_string(), Some("acc-7".to_string())));
    let api_key_only = r#"{"auth_mode": "api_key", "OPENAI_API_KEY": "sk-x"}"#;
    assert!(parse_codex_auth(api_key_only).unwrap_err().contains("API-key"));
  }

  /// Trimmed live response from `cli-chat-proxy.grok.com/v1/billing?format=credits`.
  const GROK_BILLING: &str = r#"{"config": {
    "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY",
                      "start": "2026-07-21T14:56:06Z", "end": "2026-07-28T14:56:06Z"},
    "creditUsagePercent": 3.0,
    "onDemandCap": {"val": 0}, "onDemandUsed": {"val": 0},
    "productUsage": [{"product": "GrokBuild", "usagePercent": 3.0}, {"product": "GrokChat"}],
    "isUnifiedBillingUser": true
  }}"#;

  #[test]
  fn grok_billing_reports_credits_and_products() {
    let windows = parse_grok_billing(GROK_BILLING).unwrap();
    assert_eq!(windows.len(), 3);
    assert_eq!(windows[0].id, "weekly");
    assert_eq!(windows[0].used_percent, 3.0);
    assert_eq!(windows[0].resets_at.as_deref(), Some("2026-07-28T14:56:06Z"));
    assert_eq!(windows[1].id, "weekly_grokbuild");
    // A product with no usagePercent is untouched this period — 0, not missing.
    assert_eq!(windows[2].id, "weekly_grokchat");
    assert_eq!(windows[2].used_percent, 0.0);
    assert!(parse_grok_billing("{}").is_err());
  }

  #[test]
  fn grok_auth_and_subscription_parse() {
    let auth = r#"{"https://auth.x.ai::abc-uuid": {"key": "sess-5", "refresh_token": "r",
      "expires_at": "2026-07-23T17:56:00.000000Z"}}"#;
    assert_eq!(parse_grok_auth(auth).unwrap(), "sess-5");
    assert!(parse_grok_auth("{}").is_err());
    assert_eq!(
      parse_grok_subscription(r#"{"subscriptionTier": "GrokPro", "hasGrokCodeAccess": true}"#).as_deref(),
      Some("GrokPro")
    );
  }

  /// Trimmed live response from `DashboardService/GetCurrentPeriodUsage` (epoch-ms strings).
  const CURSOR_USAGE: &str = r#"{
    "billingCycleStart": "1783782193000", "billingCycleEnd": "1786460593000",
    "planUsage": {"totalSpend": 83986, "includedSpend": 40000, "bonusSpend": 43986,
                  "limit": 40000, "autoPercentUsed": 19.35, "apiPercentUsed": 90.58,
                  "totalPercentUsed": 33.59},
    "spendLimitUsage": {"individualLimit": 100, "individualRemaining": 100, "limitType": "user"},
    "displayMessage": "You've used 100% of your included usage"
  }"#;

  #[test]
  fn cursor_usage_reports_the_three_pools() {
    let windows = parse_cursor_usage(CURSOR_USAGE).unwrap();
    assert_eq!(windows.len(), 3);
    assert_eq!(windows[0].id, "billing_cycle");
    assert_eq!(windows[0].used_percent, 33.59);
    assert_eq!(windows[0].resets_at.as_deref(), Some("2026-08-11T15:03:13Z"));
    assert_eq!(windows[1].id, "auto_pool");
    assert_eq!(windows[2].id, "api_pool");
    assert_eq!(windows[2].used_percent, 90.58);
    assert!(parse_cursor_usage("{}").is_err());
  }

  #[test]
  fn cursor_plan_name_comes_from_plan_info() {
    let body = r#"{"planInfo": {"planName": "Ultra", "includedAmountCents": 40000, "price": "$200/mo"}}"#;
    assert_eq!(parse_cursor_plan(body).as_deref(), Some("Ultra"));
    assert_eq!(parse_cursor_plan("{}"), None);
  }

  #[test]
  fn opencode_is_reported_unsupported_not_guessed() {
    let q = fetch(Harness::Opencode);
    assert_eq!(q.status, "unsupported");
    assert!(q.summary.contains("opencode"));
    assert!(!SUPPORTED.contains(&Harness::Opencode));
  }

  #[test]
  fn summaries_read_like_one_line() {
    let windows = parse_claude_usage(CLAUDE_USAGE).unwrap();
    let s = summarize(Harness::Claude, Some("max"), &windows);
    assert_eq!(s, "claude (max): 5h session 3% · weekly 57% · weekly (Fable) 21% — first reset 2026-07-23 18:10 UTC");
    let none = summarize(Harness::Codex, None, &[]);
    assert!(none.starts_with("codex: no active limit windows"));
  }

  #[test]
  fn json_shape_is_stable_and_parses() {
    let rows = vec![
      HarnessQuota::ok(Harness::Claude, Some("max".into()), parse_claude_usage(CLAUDE_USAGE).unwrap()),
      HarnessQuota::down(Harness::Grok, "expired", "the stored grok session has lapsed", "run any `grok` command"),
    ];
    let out = render_json(&rows, 1_753_000_000);
    let Ok(Value::Object(root)) = json::parse(&out) else { panic!("render_json must emit valid JSON: {out}") };
    assert_eq!(get_num(&root, "ok"), Some(1.0));
    assert_eq!(get_num(&root, "total"), Some(2.0));
    assert_eq!(get_num(&root, "checked_at"), Some(1_753_000_000.0));
    let rows_json = get_arr(&root, "harnesses").unwrap();
    let Value::Object(claude) = &rows_json[0] else { panic!("object") };
    assert_eq!(get_str(claude, "status"), Some("ok"));
    assert_eq!(get_str(claude, "plan"), Some("max"));
    assert_eq!(get_arr(claude, "windows").unwrap().len(), 3);
    let Value::Object(grok) = &rows_json[1] else { panic!("object") };
    assert_eq!(get_str(grok, "status"), Some("expired"));
    assert_eq!(get_str(grok, "plan"), None);
    assert!(get_str(grok, "summary").unwrap().contains("lapsed"));
  }

  #[test]
  fn result_file_headline_is_the_summary_line() {
    let q = HarnessQuota::ok(Harness::Claude, Some("max".into()), parse_claude_usage(CLAUDE_USAGE).unwrap());
    let file = result_json(&q);
    // The run's result file leads with `result`, so json::message — which the job page
    // uses for the proc headline — yields the human status line, never a path.
    assert_eq!(json::message(&file).as_deref(), Some(q.summary.as_str()));
    let Ok(Value::Object(root)) = json::parse(&file) else { panic!("result_json must be valid JSON: {file}") };
    assert_eq!(get_str(&root, "harness"), Some("claude"));
    assert_eq!(get_str(&root, "status"), Some("ok"));
    assert_eq!(get_arr(&root, "windows").unwrap().len(), 3);
    assert_eq!(run_name(Harness::Cursor), "quota-cursor");
    assert_eq!(result_file_name(Harness::Cursor), "quota-cursor.json");
    // Failed checks still write a result file whose headline says what went wrong.
    let down = HarnessQuota::down(Harness::Grok, "expired", "the stored grok session has lapsed", "run `grok`");
    assert!(json::message(&result_json(&down)).unwrap().contains("lapsed"));
  }

  #[test]
  fn table_aligns_and_groups_by_harness() {
    let rows = vec![
      HarnessQuota::ok(Harness::Claude, Some("max".into()), parse_claude_usage(CLAUDE_USAGE).unwrap()),
      HarnessQuota::down(Harness::Codex, "missing", "no ChatGPT login found", "run `codex login`"),
    ];
    let table = render_table(&rows);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 5); // header + 3 claude windows + 1 codex status row
    assert!(lines[0].starts_with("harness"));
    assert!(lines[1].starts_with("claude"));
    // Continuation rows leave the harness/plan columns blank.
    assert!(lines[2].starts_with("  "));
    assert!(lines[4].contains("(missing)"));
    // `used` is right-aligned: the single-digit gauge ends where the two-digit one does.
    let col = |line: &str, pat: &str| line.find(pat).unwrap() + pat.len();
    assert_eq!(col(lines[1], "3%"), col(lines[2], "57%"));
  }

  #[test]
  fn timestamps_normalize_to_utc_z() {
    assert_eq!(normalize_iso("2026-07-23T18:10:00+00:00"), "2026-07-23T18:10:00Z");
    assert_eq!(normalize_iso("2026-07-23T17:56:00.057655Z"), "2026-07-23T17:56:00Z");
    assert_eq!(normalize_iso("2026-07-28T14:56:06Z"), "2026-07-28T14:56:06Z");
    assert_eq!(epoch_to_iso(1_785_258_441), "2026-07-28T17:07:21Z");
    assert_eq!(human_time("2026-07-26T16:00:00Z"), "2026-07-26 16:00");
    assert_eq!(trim_percent(3.0), "3");
    assert_eq!(trim_percent(33.59), "33.6");
    assert_eq!(slug("GPT-5.3-Codex-Spark"), "gpt_5_3_codex_spark");
  }

  #[test]
  fn curl_config_values_are_quoted() {
    assert_eq!(curl_config_quote("https://x/y"), "\"https://x/y\"");
    assert_eq!(curl_config_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
  }
}
