//! JSON read/write for daemon state — std-only, no serde.

use super::model::{OutputLine, ProcKind, ProcRecord, ProcStatus, Session, SkillMeta, Store};
use crate::json::{parse, quote, Value};

fn sessions_json(map: &std::collections::BTreeMap<String, Session>) -> String {
  if map.is_empty() {
    return "{}".to_string();
  }
  let mut parts = Vec::new();
  for (id, s) in map {
    parts.push(format!("{}: {}", quote(id), session_json(s, true)));
  }
  format!("{{ {} }}", parts.join(", "))
}

pub fn session_json_api(s: &Session) -> String {
  // Live view for HTTP + WebSocket ticks: include the synthesized job graph (builds + skills).
  session_json(s, true)
}

/// Persistable session JSON (authored `workflow` only — no synthesized build nodes).
pub fn session_json_store(s: &Session) -> String {
  session_json(s, false)
}

/// Parse one session from its JSON (the inverse of [`session_json_store`]) — how the daemon's
/// redb store deserializes each per-session record on load.
pub fn parse_session_json(text: &str) -> Result<Session, String> {
  parse_session(&parse(text)?)
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

/// WebSocket push when a running proc's recording grew — or, with `"running": false`, the
/// final notice when the proc stops (so clients end live mode and load the complete cast).
/// `duration` is the cast's available duration in seconds, derived server-side from the
/// asciicast NDJSON tail (see `castprobe`).
pub fn cast_growth_json(session: &str, proc: usize, duration: f64, running: bool) -> String {
  format!(
    "{{ \"type\": \"cast_growth\", \"session\": {}, \"proc\": {proc}, \"duration\": {}, \"running\": {} }}",
    quote(session),
    // `max` would swallow a NaN (NaN.max(0.0) == 0.0), so clamp only finite values.
    format_f64_json(if duration.is_finite() { duration.max(0.0) } else { duration }),
    if running { "true" } else { "false" },
  )
}

fn session_json(s: &Session, effective_workflow: bool) -> String {
  let profile = match &s.profile {
    Some(p) => quote(p),
    None => "null".to_string(),
  };
  let kind = match &s.kind {
    Some(k) => quote(k),
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
  let run_pid = match s.run_pid {
    Some(p) => format!("{p}"),
    None => "null".to_string(),
  };
  let workflow = if effective_workflow {
    match super::workflow::effective_workflow_meta(s) {
      Some(w) => format!(", \"workflow\": {}", super::workflow::workflow_json(&w)),
      None => String::new(),
    }
  } else {
    match &s.workflow {
      Some(w) => format!(", \"workflow\": {}", super::workflow::workflow_json(w)),
      None => String::new(),
    }
  };
  let workflow_loops = if effective_workflow {
    let plans = super::workflow::workflow_loop_plans(s);
    if plans.is_empty() {
      String::new()
    } else {
      format!(", \"workflow_loops\": {}", super::workflow::workflow_loop_plans_json(&plans))
    }
  } else {
    String::new()
  };
  let parent_session = match &s.parent_session {
    Some(p) => format!(", \"parent_session\": {}", quote(p)),
    None => String::new(),
  };
  format!(
    "{{ \"id\": {}, \"started_at\": {}, \"ended_at\": {ended_at}, \"profile\": {}, \"kind\": {}, \"repo\": {}, \
\"branch\": {}, \"skills\": [{}], \"procs\": [{}], \"last_seen_at\": {}, \"client_connected\": {}, \
\"run_pid\": {run_pid}{workflow}{workflow_loops}{parent_session} }}",
    quote(&s.id),
    s.started_at,
    profile,
    kind,
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
  let cast = opt_str(&p.cast_path);
  let diff = opt_str(&p.diff_path);
  let lines: Vec<String> =
    p.lines.iter().map(|l| format!("{{ \"at\": {}, \"text\": {} }}", format_f64_json(l.at), quote(&l.text))).collect();
  let started_at = match p.started_at {
    Some(s) => format!("{s}"),
    None => "null".to_string(),
  };
  format!(
    "{{ \"index\": {}, \"label\": {}, \"kind\": {}, \"status\": {}, \"skill_name\": {}, \
\"harness\": {}, \"model\": {}, \"started_at\": {started_at}, \"note\": {}, \"detail\": {}, \"fail_reason\": {}, \
\"elapsed\": {}, \"container_name\": {}, \"cast_path\": {}, \"diff_path\": {}, \
\"skill_source\": {}, \"route\": {}, \"result_path\": {}, \"annotate_target\": {}, \"lines\": [{}] }}",
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
    cast,
    diff,
    opt_str(&p.skill_source),
    opt_str(&p.route),
    opt_str(&p.result_path),
    opt_str(&p.annotate_target),
    lines.join(", ")
  )
}

fn opt_str(s: &Option<String>) -> String {
  match s {
    Some(v) => quote(v),
    None => "null".to_string(),
  }
}

fn parse_session(v: &Value) -> Result<Session, String> {
  let obj = as_object(v)?;
  let id = field_str(obj, "id").unwrap_or_default();
  let started_at = field_num(obj, "started_at").unwrap_or(0.0) as u64;
  let ended_at = field_num(obj, "ended_at").and_then(|n| if n > 0.0 { Some(n as u64) } else { None });
  let profile = field_str(obj, "profile");
  let kind = field_str(obj, "kind"); // absent on sessions persisted by older builds
  let repo = field_str(obj, "repo").unwrap_or_default();
  let branch = field_str(obj, "branch").unwrap_or_default();
  let skills = parse_skills(field_value(obj, "skills").ok());
  let procs = match field_value(obj, "procs")? {
    Value::Array(arr) => arr.iter().map(parse_proc).collect::<Result<Vec<_>, _>>()?,
    _ => Vec::new(),
  };
  let last_seen_at = field_num(obj, "last_seen_at").map(|n| n as u64).unwrap_or(started_at);
  let client_connected = field_bool(obj, "client_connected").unwrap_or(false);
  let run_pid = field_num(obj, "run_pid").and_then(|n| if n > 0.0 { Some(n as u32) } else { None });
  let workflow = super::workflow::parse_workflow_value(field_value(obj, "workflow").ok());
  let parent_session = field_str(obj, "parent_session");
  Ok(Session {
    id,
    started_at,
    ended_at,
    profile,
    kind,
    repo,
    branch,
    skills,
    procs,
    last_seen_at,
    client_connected,
    run_pid,
    workflow,
    parent_session,
  })
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
  let cast_path = field_str(obj, "cast_path");
  let diff_path = field_str(obj, "diff_path"); // absent on sessions persisted by older builds
  let skill_source = field_str(obj, "skill_source");
  let route = field_str(obj, "route");
  let result_path = field_str(obj, "result_path");
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
    cast_path,
    diff_path,
    skill_source,
    route,
    result_path,
    annotate_target: field_str(obj, "annotate_target"), // absent on sessions persisted by older builds
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
      cast_path: None,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
    };
    let json = proc_json(&proc);
    assert!(!json.contains("NaN"));
    assert!(!json.contains("Infinity"));
    assert!(json.contains("\"elapsed\": null"));
    assert!(json.contains("\"at\": null"));
  }

  #[test]
  fn roundtrip_session_with_proc() {
    let mut session = Session {
      id: "abcdef".into(),
      started_at: 99,
      ended_at: Some(105),
      profile: Some("default".into()),
      kind: None,
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
        cast_path: Some("/tmp/scsh-daemon/casts/abcdef-p0.cast".into()),
        diff_path: Some("/tmp/scsh-home/sessions/abcdef/diffs/add.html".into()),
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
        lines: vec![OutputLine { at: 0.1, text: "step 1".into() }],
      }],
      last_seen_at: 105,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
    };
    // The per-session JSON the store DB reads/writes must roundtrip every field.
    let s = parse_session_json(&session_json_store(&session)).unwrap();
    assert_eq!(s.procs[0].lines[0].text, "step 1");
    assert_eq!(s.procs[0].detail.as_deref(), Some("up to date"));
    assert_eq!(s.procs[0].cast_path.as_deref(), Some("/tmp/scsh-daemon/casts/abcdef-p0.cast"));
    assert_eq!(s.procs[0].diff_path.as_deref(), Some("/tmp/scsh-home/sessions/abcdef/diffs/add.html"));
    assert_eq!(s.ended_at, Some(105));
    assert_eq!(s.skills[0].name, "add");
    assert_eq!(s.parent_session, None);

    session.parent_session = Some("parent1".into());
    let with_parent = parse_session_json(&session_json_store(&session)).unwrap();
    assert_eq!(with_parent.parent_session.as_deref(), Some("parent1"));
    assert!(session_json_store(&session).contains("\"parent_session\": \"parent1\""));
  }

  #[test]
  fn roundtrip_workflow_graph_and_legacy_without_it() {
    use crate::daemon::workflow::{WorkflowMeta, WorkflowNodeMeta};
    let mut session = Session {
      id: "wf0001".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("arith".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![],
      last_seen_at: 1,
      client_connected: false,
      run_pid: None,
      workflow: Some(WorkflowMeta {
        nodes: vec![
          WorkflowNodeMeta {
            id: "add".into(),
            proc_index: Some(0),
            order: 0,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "summarize".into(),
            proc_index: None,
            order: 1,
            needs: vec!["add".into()],
            conditional: false,
            when_summary: None,
          },
        ],
      }),
      parent_session: None,
    };
    let parsed = parse_session_json(&session_json_store(&session)).unwrap();
    assert_eq!(parsed.workflow.as_ref().unwrap().nodes.len(), 2);
    assert_eq!(parsed.workflow.as_ref().unwrap().nodes[1].needs, vec!["add".to_string()]);
    session.workflow = None;
    let legacy = parse_session_json(&session_json_store(&session)).unwrap();
    assert!(legacy.workflow.is_none());
    // Live API synthesizes a graph from skills/procs even without authored workflow.
    let live = session_json_api(&session);
    assert!(!live.contains(", \"workflow\":"), "empty session has no effective graph: {live}");
    // Missing key entirely (older builds)
    let bare = r#"{ "id": "old", "started_at": 1, "ended_at": null, "profile": null, "kind": null, "repo": "/r", "branch": "main", "skills": [], "procs": [], "last_seen_at": 1, "client_connected": false, "run_pid": null }"#;
    assert!(parse_session_json(bare).unwrap().workflow.is_none());
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
  fn cast_growth_json_shape_matches_ws_schema() {
    assert_eq!(
      cast_growth_json("abcdef", 2, 12.5, true),
      r#"{ "type": "cast_growth", "session": "abcdef", "proc": 2, "duration": 12.5, "running": true }"#
    );
    assert_eq!(
      cast_growth_json("abcdef", 0, 0.0, false),
      r#"{ "type": "cast_growth", "session": "abcdef", "proc": 0, "duration": 0, "running": false }"#
    );
    // Escaping and non-finite durations follow the same rules as the tick payload.
    assert!(cast_growth_json("a\"b", 1, f64::NAN, true).contains("\"duration\": null"));
    assert!(cast_growth_json("a\"b", 1, 1.0, true).contains(r#""session": "a\"b""#));
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
        kind: None,
        repo: "/tmp".into(),
        branch: "main".into(),
        skills: vec![],
        procs: vec![],
        last_seen_at: 100,
        client_connected: false,
        run_pid: None,
        workflow: None,
        parent_session: None,
      },
    );
    let light = tick_json_light(&store, 105);
    assert!(!light.contains("\"sessions\""));
    let full = tick_json(&store, 105);
    assert!(full.contains("\"sessions\""));
  }
}
