//! JSON read/write for daemon state — std-only, no serde.

use super::model::{
  trim_sessions_to_cap, DaemonMode, OutputLine, ProcKind, ProcRecord, ProcStatus, Session, SkillMeta, Store,
  DEFAULT_PORT,
};
use crate::json::{parse, quote, Value};

pub fn load_store(text: &str) -> Result<Store, String> {
  let root = parse(text)?;
  let obj = as_object(&root)?;
  let mode =
    DaemonMode::parse(field_str(obj, "mode").as_deref().unwrap_or("ephemeral")).unwrap_or(DaemonMode::Ephemeral);
  let port = field_num(obj, "port").unwrap_or(DEFAULT_PORT as f64) as u16;
  let started_at = field_num(obj, "started_at").unwrap_or(0.0) as u64;
  let active_clients = field_num(obj, "active_clients").unwrap_or(0.0) as u32;
  let last_activity = field_num(obj, "last_activity").unwrap_or(0.0) as u64;
  let no_alive_since = field_num(obj, "no_alive_since").and_then(|n| if n > 0.0 { Some(n as u64) } else { None });
  let mut sessions = parse_sessions(field_value(obj, "sessions")?)?;
  trim_sessions_to_cap(&mut sessions);
  Ok(Store { mode, port, started_at, active_clients, last_activity, no_alive_since, sessions })
}

pub fn save_store(store: &Store) -> String {
  let mut out = String::from("{\n");
  out.push_str(&format!("  \"mode\": {},\n", quote(store.mode.as_str())));
  out.push_str(&format!("  \"port\": {},\n", store.port));
  out.push_str(&format!("  \"started_at\": {},\n", store.started_at));
  out.push_str(&format!("  \"active_clients\": {},\n", store.active_clients));
  out.push_str(&format!("  \"last_activity\": {},\n", store.last_activity));
  out.push_str(&format!(
    "  \"no_alive_since\": {},\n",
    match store.no_alive_since {
      Some(t) => format!("{t}"),
      None => "null".to_string(),
    }
  ));
  out.push_str("  \"sessions\": ");
  out.push_str(&sessions_json(&store.sessions));
  out.push_str("\n}");
  out
}

fn sessions_json(map: &std::collections::BTreeMap<String, Session>) -> String {
  if map.is_empty() {
    return "{}".to_string();
  }
  let mut parts = Vec::new();
  for (id, s) in map {
    parts.push(format!("{}: {}", quote(id), session_json(s)));
  }
  format!("{{ {} }}", parts.join(", "))
}

pub fn session_json_api(s: &Session) -> String {
  session_json(s)
}

/// WebSocket tick payload: daemon status + full session snapshot.
pub fn tick_json(store: &Store, now: u64) -> String {
  tick_json_with_sessions(store, now, true)
}

/// Lightweight tick for idle periods: status fields only (no `sessions` blob).
pub fn tick_json_light(store: &Store, now: u64) -> String {
  tick_json_with_sessions(store, now, false)
}

fn tick_json_with_sessions(store: &Store, now: u64, include_sessions: bool) -> String {
  let uptime = now.saturating_sub(store.started_at);
  let git = crate::version::git_stamp();
  let git_json = if git.is_empty() { "null".to_string() } else { quote(&git) };
  let alive = store.alive_clients(now);
  let shutdown_in = store.ephemeral_shutdown_in_secs(now);
  let shutdown_json = match shutdown_in {
    Some(t) => format!("{t}"),
    None => "null".to_string(),
  };
  let sessions_part =
    if include_sessions { format!(", \"sessions\": {}", sessions_json(&store.sessions)) } else { String::new() };
  format!(
    "{{ \"type\": \"tick\", \"now_secs\": {now}, \"uptime_secs\": {uptime}, \"mode\": {}, \"port\": {}, \
\"active_clients\": {}, \"alive_clients\": {alive}, \"shutdown_in_secs\": {shutdown_json}, \"scsh_version\": {}, \
\"scsh_git\": {git_json}{sessions_part} }}",
    quote(store.mode.as_str()),
    store.port,
    store.active_clients,
    quote(crate::version::pkg_version()),
  )
}

fn session_json(s: &Session) -> String {
  let profile = match &s.profile {
    Some(p) => quote(p),
    None => "null".to_string(),
  };
  let skills: Vec<String> = s
    .skills
    .iter()
    .map(|sk| format!("{{ \"name\": {}, \"harness\": {} }}", quote(&sk.name), quote(&sk.harness)))
    .collect();
  let procs: Vec<String> = s.procs.iter().map(proc_json).collect();
  let ended_at = match s.ended_at {
    Some(t) => format!("{t}"),
    None => "null".to_string(),
  };
  format!(
    "{{ \"id\": {}, \"started_at\": {}, \"ended_at\": {ended_at}, \"profile\": {}, \"repo\": {}, \
\"branch\": {}, \"skills\": [{}], \"procs\": [{}], \"last_seen_at\": {}, \"client_connected\": {} }}",
    quote(&s.id),
    s.started_at,
    profile,
    quote(&s.repo),
    quote(&s.branch),
    skills.join(", "),
    procs.join(", "),
    s.last_seen_at,
    if s.client_connected { "true" } else { "false" },
  )
}

fn format_f64_json(v: f64) -> String {
  if v.is_finite() {
    format!("{v}")
  } else {
    "null".to_string()
  }
}

fn proc_json(p: &ProcRecord) -> String {
  let note = opt_str(&p.note);
  let detail = opt_str(&p.detail);
  let elapsed = match p.elapsed {
    Some(e) => format_f64_json(e),
    None => "null".to_string(),
  };
  let container = opt_str(&p.container_name);
  let lines: Vec<String> =
    p.lines.iter().map(|l| format!("{{ \"at\": {}, \"text\": {} }}", format_f64_json(l.at), quote(&l.text))).collect();
  let started_at = match p.started_at {
    Some(s) => format!("{s}"),
    None => "null".to_string(),
  };
  format!(
    "{{ \"index\": {}, \"label\": {}, \"kind\": {}, \"status\": {}, \"skill_name\": {}, \
\"harness\": {}, \"model\": {}, \"started_at\": {started_at}, \"note\": {}, \"detail\": {}, \"fail_reason\": {}, \
\"elapsed\": {}, \"container_name\": {}, \"lines\": [{}] }}",
    p.index,
    quote(&p.label),
    quote(p.kind.as_str()),
    quote(p.status.as_str()),
    opt_str(&p.skill_name),
    opt_str(&p.harness),
    opt_str(&p.model),
    note,
    detail,
    opt_str(&p.fail_reason),
    elapsed,
    container,
    lines.join(", ")
  )
}

fn opt_str(s: &Option<String>) -> String {
  match s {
    Some(v) => quote(v),
    None => "null".to_string(),
  }
}

fn parse_sessions(v: &Value) -> Result<std::collections::BTreeMap<String, Session>, String> {
  let obj = as_object(v)?;
  let mut out = std::collections::BTreeMap::new();
  for (id, val) in obj {
    out.insert(id.clone(), parse_session(val)?);
  }
  Ok(out)
}

fn parse_session(v: &Value) -> Result<Session, String> {
  let obj = as_object(v)?;
  let id = field_str(obj, "id").unwrap_or_default();
  let started_at = field_num(obj, "started_at").unwrap_or(0.0) as u64;
  let ended_at = field_num(obj, "ended_at").and_then(|n| if n > 0.0 { Some(n as u64) } else { None });
  let profile = field_str(obj, "profile");
  let repo = field_str(obj, "repo").unwrap_or_default();
  let branch = field_str(obj, "branch").unwrap_or_default();
  let skills = parse_skills(field_value(obj, "skills").ok());
  let procs = match field_value(obj, "procs")? {
    Value::Array(arr) => arr.iter().map(parse_proc).collect::<Result<Vec<_>, _>>()?,
    _ => Vec::new(),
  };
  let last_seen_at = field_num(obj, "last_seen_at").map(|n| n as u64).unwrap_or(started_at);
  let client_connected = field_bool(obj, "client_connected").unwrap_or(false);
  Ok(Session { id, started_at, ended_at, profile, repo, branch, skills, procs, last_seen_at, client_connected })
}

fn parse_skills(v: Option<&Value>) -> Vec<SkillMeta> {
  let empty = Value::Array(Vec::new());
  let v = v.unwrap_or(&empty);
  let Value::Array(arr) = v else {
    return Vec::new();
  };
  arr
    .iter()
    .filter_map(|item| {
      let obj = as_object(item).ok()?;
      Some(SkillMeta {
        name: field_str(obj, "name").unwrap_or_default(),
        harness: field_str(obj, "harness").unwrap_or_default(),
      })
    })
    .collect()
}

fn parse_proc(v: &Value) -> Result<ProcRecord, String> {
  let obj = as_object(v)?;
  let index = field_num(obj, "index").unwrap_or(0.0) as usize;
  let label = field_str(obj, "label").unwrap_or_default();
  let kind = ProcKind::parse(field_str(obj, "kind").as_deref().unwrap_or("skill")).unwrap_or(ProcKind::Skill);
  let status =
    ProcStatus::parse(field_str(obj, "status").as_deref().unwrap_or("waiting")).unwrap_or(ProcStatus::Waiting);
  let note = field_str(obj, "note");
  let detail = field_str(obj, "detail");
  let fail_reason = field_str(obj, "fail_reason");
  let elapsed = field_num(obj, "elapsed");
  let container_name = field_str(obj, "container_name");
  let lines = match field_value(obj, "lines")? {
    Value::Array(arr) => arr.iter().map(parse_line).collect::<Result<Vec<_>, _>>()?,
    _ => Vec::new(),
  };
  Ok(ProcRecord {
    index,
    label,
    kind,
    status,
    skill_name: field_str(obj, "skill_name"),
    harness: field_str(obj, "harness"),
    model: field_str(obj, "model"),
    started_at: field_num(obj, "started_at").map(|n| n as u64),
    note,
    detail,
    fail_reason,
    elapsed,
    container_name,
    lines,
  })
}

fn parse_line(v: &Value) -> Result<OutputLine, String> {
  let obj = as_object(v)?;
  let at = field_num(obj, "at").unwrap_or(0.0);
  let text = field_str(obj, "text").unwrap_or_default();
  Ok(OutputLine { at, text })
}

fn as_object(v: &Value) -> Result<&[(String, Value)], String> {
  match v {
    Value::Object(o) => Ok(o),
    _ => Err("expected object".into()),
  }
}

fn field_value<'a>(obj: &'a [(String, Value)], key: &str) -> Result<&'a Value, String> {
  obj.iter().find(|(k, _)| k == key).map(|(_, v)| v).ok_or_else(|| format!("missing field '{key}'"))
}

pub(crate) fn field_str(obj: &[(String, Value)], key: &str) -> Option<String> {
  match field_value(obj, key) {
    Ok(Value::String(s)) => Some(s.clone()),
    Ok(Value::Null) => None,
    Ok(_) => None,
    Err(_) => None,
  }
}

pub(crate) fn field_num(obj: &[(String, Value)], key: &str) -> Option<f64> {
  match field_value(obj, key) {
    Ok(Value::Number(n)) => Some(*n),
    _ => None,
  }
}

pub(crate) fn field_bool(obj: &[(String, Value)], key: &str) -> Option<bool> {
  match field_value(obj, key) {
    Ok(Value::Bool(b)) => Some(*b),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::daemon::model::{DaemonMode, ProcKind, ProcStatus, EPHEMERAL_COUNTDOWN_AFTER_SECS};

  #[test]
  fn proc_json_serializes_non_finite_as_null() {
    let proc = ProcRecord {
      index: 0,
      label: "skill".into(),
      kind: ProcKind::Skill,
      status: ProcStatus::Ok,
      skill_name: None,
      harness: None,
      model: None,
      started_at: None,
      note: None,
      detail: None,
      fail_reason: None,
      elapsed: Some(f64::NAN),
      lines: vec![OutputLine { at: f64::INFINITY, text: "x".into() }],
      container_name: None,
    };
    let json = proc_json(&proc);
    assert!(!json.contains("NaN"));
    assert!(!json.contains("Infinity"));
    assert!(json.contains("\"elapsed\": null"));
    assert!(json.contains("\"at\": null"));
  }

  #[test]
  fn roundtrip_empty_store() {
    let store = Store::new(DaemonMode::Persistent, 7274, 42);
    let loaded = load_store(&save_store(&store)).unwrap();
    assert_eq!(loaded.mode, DaemonMode::Persistent);
    assert_eq!(loaded.port, 7274);
    assert_eq!(loaded.last_activity, 42);
    assert!(loaded.sessions.is_empty());
  }

  #[test]
  fn roundtrip_session_with_proc() {
    let mut store = Store::new(DaemonMode::Ephemeral, 7274, 1);
    let session = Session {
      id: "abcdef".into(),
      started_at: 99,
      ended_at: Some(105),
      profile: Some("default".into()),
      repo: "/tmp/repo".into(),
      branch: "main".into(),
      skills: vec![SkillMeta { name: "add".into(), harness: "opencode".into() }],
      procs: vec![ProcRecord {
        index: 0,
        label: "build".into(),
        kind: ProcKind::Build,
        status: ProcStatus::Ok,
        skill_name: None,
        harness: Some("opencode".into()),
        model: None,
        started_at: Some(99),
        note: None,
        detail: Some("up to date".into()),
        fail_reason: None,
        elapsed: Some(1.5),
        container_name: None,
        lines: vec![OutputLine { at: 0.1, text: "step 1".into() }],
      }],
      last_seen_at: 105,
      client_connected: false,
    };
    store.sessions.insert(session.id.clone(), session);
    let loaded = load_store(&save_store(&store)).unwrap();
    let s = loaded.sessions.get("abcdef").unwrap();
    assert_eq!(s.procs[0].lines[0].text, "step 1");
    assert_eq!(s.procs[0].detail.as_deref(), Some("up to date"));
    assert_eq!(s.ended_at, Some(105));
  }

  #[test]
  fn tick_json_includes_alive_clients_and_shutdown() {
    let store = Store::new(DaemonMode::Ephemeral, 7274, 100);
    let json = tick_json(&store, 100 + EPHEMERAL_COUNTDOWN_AFTER_SECS);
    assert!(json.contains("\"alive_clients\": 0"));
    assert!(json.contains("\"shutdown_in_secs\""));
    assert!(json.contains("\"scsh_version\""));
    assert!(json.contains(crate::version::pkg_version()));
  }

  #[test]
  fn tick_json_light_omits_sessions_blob() {
    let mut store = Store::new(DaemonMode::Persistent, 7274, 100);
    store.sessions.insert(
      "abcdef".into(),
      Session {
        id: "abcdef".into(),
        started_at: 100,
        ended_at: None,
        profile: None,
        repo: "/tmp".into(),
        branch: "main".into(),
        skills: vec![],
        procs: vec![],
        last_seen_at: 100,
        client_connected: false,
      },
    );
    let light = tick_json_light(&store, 105);
    assert!(!light.contains("\"sessions\""));
    let full = tick_json(&store, 105);
    assert!(full.contains("\"sessions\""));
  }
}
