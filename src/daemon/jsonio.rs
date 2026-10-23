//! JSON read/write for daemon state — std-only, no serde.

use super::model::{
  OutputLine, ProcKind, ProcRecord, ProcStatus, ReportEntry, ReportSection, Session, SessionLifecycle, SkillMeta, Store,
};
use crate::json::{parse, quote, Value};

fn sessions_json(store: &Store, now: u64) -> String {
  if store.sessions.is_empty() {
    return "{}".to_string();
  }
  let mut parts = Vec::new();
  for (id, s) in &store.sessions {
    parts.push(format!("{}: {}", quote(id), session_json(s, true, Some(store.lifecycle_of(s, now)))));
  }
  format!("{{ {} }}", parts.join(", "))
}

/// Live view for HTTP + WebSocket ticks: the synthesized job graph (builds + skills) and
/// the daemon-authoritative lifecycle, which needs the store to see annotate children.
pub fn session_json_api(store: &Store, s: &Session) -> String {
  session_json(s, true, Some(store.lifecycle_of(s, crate::daemon::paths::now_unix_secs())))
}

/// The live view of an archived session: evicted from the store, so no child session can
/// still be annotating it and its own record decides the lifecycle.
pub fn archived_session_json_api(s: &Session) -> String {
  session_json(s, true, Some(s.lifecycle_status(crate::daemon::paths::now_unix_secs())))
}

/// Persistable session JSON (authored `workflow` only — no synthesized build nodes).
pub fn session_json_store(s: &Session) -> String {
  session_json(s, false, None)
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
    if include_sessions { format!(", \"sessions\": {}", sessions_json(store, now)) } else { String::new() };
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

/// One `transcript_growth` WebSocket message: a headless proc's event stream grew (or, with
/// `running: false`, stopped). It carries no content: the page re-fetches the rendered
/// transcript (see `castprobe`).
pub fn transcript_growth_json(session: &str, proc: usize, running: bool) -> String {
  format!(
    "{{ \"type\": \"transcript_growth\", \"session\": {}, \"proc\": {proc}, \"running\": {} }}",
    quote(session),
    if running { "true" } else { "false" },
  )
}

fn session_json(s: &Session, effective_workflow: bool, lifecycle: Option<SessionLifecycle>) -> String {
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
  // Supervisor state rides only when it says something (records persisted before the
  // retries budget existed parse back to the zero-budget default without it).
  let supervisor = if s.supervisor == Default::default() {
    String::new()
  } else {
    let sup = &s.supervisor;
    format!(
      ", \"supervisor\": {{ \"job_attempt\": {}, \"retries\": {}, \
\"next_retry_at\": {}, \"fail_signature\": {}, \"fail_streak\": {}, \"gave_up\": {}, \"restarted_as\": {} }}",
      sup.job_attempt,
      sup.retries,
      sup.next_retry_at.map_or_else(|| "null".to_string(), |t| t.to_string()),
      opt_str(&sup.fail_signature),
      sup.fail_streak,
      opt_str(&sup.gave_up),
      opt_str(&sup.restarted_as),
    )
  };
  let lifecycle = lifecycle.map(|state| {
    format!(", \"lifecycle\": {}, \"lifecycle_label\": {}", quote(state.css_class()), quote(state.label()))
  });
  // The live view carries each contribution rendered and already in page order, so the page
  // can mount it as it lands without a renderer or a sort of its own; the store keeps only the
  // markdown, in arrival order (the page order is derived from the tasks' keys).
  let report = if s.report.is_empty() {
    String::new()
  } else {
    let entries: Vec<String> = if effective_workflow {
      s.report_in_page_order().into_iter().map(|e| report_entry_json(e, Some(s.report_order_of(e)))).collect()
    } else {
      s.report.iter().map(|e| report_entry_json(e, None)).collect()
    };
    format!(", \"report\": [{}]", entries.join(", "))
  };
  format!(
    "{{ \"id\": {}, \"started_at\": {}, \"ended_at\": {ended_at}, \"profile\": {}, \"kind\": {}, \"repo\": {}, \
\"branch\": {}, \"skills\": [{}], \"procs\": [{}], \"last_seen_at\": {}, \"client_connected\": {}, \
\"run_pid\": {run_pid}{workflow}{workflow_loops}{parent_session}{supervisor}{report}{lifecycle} }}",
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
    lifecycle = lifecycle.unwrap_or_default(),
  )
}

/// One contribution. `live` (the writing task's report-order key) adds what only the live view
/// carries: the rendered HTML and the key's dotted form.
fn report_entry_json(e: &ReportEntry, live: Option<&[u32]>) -> String {
  let proc = e.proc.map_or_else(|| "null".to_string(), |p| p.to_string());
  let html = match live {
    Some(order) => format!(
      ", \"html\": {}, \"order\": {}",
      quote(&super::html::markdown_to_html(&e.markdown)),
      quote(&super::html::report::order_label(order))
    ),
    None => String::new(),
  };
  format!(
    "{{ \"section\": {}, \"proc\": {proc}, \"source\": {}, \"markdown\": {}{html} }}",
    quote(e.section.as_str()),
    quote(&e.source),
    quote(&e.markdown),
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
  let previous_attempt = p.previous_attempt.map_or_else(|| "null".to_string(), |index| index.to_string());
  let note = opt_str(&p.note);
  let detail = opt_str(&p.detail);
  let elapsed = match p.elapsed {
    Some(e) => format_f64_json(e),
    None => "null".to_string(),
  };
  let container = opt_str(&p.container_name);
  let container_runtime = opt_str(&p.container_runtime);
  let cast = opt_str(&p.cast_path);
  let transcript = opt_str(&p.transcript_path);
  let diff = opt_str(&p.diff_path);
  let lines: Vec<String> =
    p.lines.iter().map(|l| format!("{{ \"at\": {}, \"text\": {} }}", format_f64_json(l.at), quote(&l.text))).collect();
  let started_at = match p.started_at {
    Some(s) => format!("{s}"),
    None => "null".to_string(),
  };
  let recovery = p.recovery.map(|state| state.json()).unwrap_or_else(|| "null".into());
  let phase_until = match p.phase_until {
    Some(s) => format!("{s}"),
    None => "null".to_string(),
  };
  let suspected_cause = p.suspected_cause.map(|cause| quote(cause.as_str())).unwrap_or_else(|| "null".into());
  let usage = p.usage.as_ref().map(crate::usage::Summary::compact_json).unwrap_or_else(|| "null".into());
  let quota = p.quota.as_ref().map(crate::quota::RunQuota::compact_json).unwrap_or_else(|| "null".into());
  let order = order_json(&p.order);
  format!(
    "{{ \"index\": {}, \"previous_attempt\": {previous_attempt}, \"order\": {order}, \"label\": {},\"kind\": {}, \"status\": {}, \"skill_name\": {}, \
\"harness\": {}, \"model\": {}, \"started_at\": {started_at}, \"note\": {}, \"detail\": {}, \"fail_reason\": {}, \"suspected_cause\": {suspected_cause}, \
\"elapsed\": {}, \"container_name\": {}, \"container_runtime\": {}, \"cast_path\": {}, \"transcript_path\": {transcript}, \"diff_path\": {}, \
\"skill_source\": {}, \"route\": {}, \"result_path\": {}, \"annotate_target\": {}, \"phase\": {}, \
\"phase_until\": {phase_until}, \"recovery\": {recovery}, \"usage\": {}, \"quota\": {quota}, \"lines\": [{}] }}",
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
    container_runtime,
    cast,
    diff,
    opt_str(&p.skill_source),
    opt_str(&p.route),
    opt_str(&p.result_path),
    opt_str(&p.annotate_target),
    opt_str(&p.phase),
    usage,
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
  let supervisor = parse_supervisor(field_value(obj, "supervisor").ok());
  let report = match field_value(obj, "report") {
    Ok(Value::Array(arr)) => arr.iter().filter_map(parse_report_entry).collect(),
    _ => Vec::new(),
  };
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
    supervisor,
    report,
  })
}

/// One report entry; `None` for a malformed one (an unknown section, no text), which is
/// dropped rather than failing the whole session.
fn parse_report_entry(v: &Value) -> Option<ReportEntry> {
  let obj = as_object(v).ok()?;
  let section = ReportSection::parse(&field_str(obj, "section")?)?;
  let markdown = field_str(obj, "markdown")?;
  let proc = field_num(obj, "proc").map(|n| n as usize);
  let source = field_str(obj, "source").unwrap_or_default();
  Some(ReportEntry { section, proc, source, markdown })
}

/// Supervisor state, absent on records persisted before it existed (⇒ attended default).
fn parse_supervisor(v: Option<&Value>) -> super::model::SupervisorState {
  let Some(Value::Object(obj)) = v else {
    return Default::default();
  };
  super::model::SupervisorState {
    job_attempt: field_num(obj, "job_attempt").unwrap_or(0.0) as u32,
    retries: field_num(obj, "retries").unwrap_or(0.0) as u32,
    next_retry_at: field_num(obj, "next_retry_at").and_then(|n| if n > 0.0 { Some(n as u64) } else { None }),
    fail_signature: field_str(obj, "fail_signature"),
    fail_streak: field_num(obj, "fail_streak").unwrap_or(0.0) as u32,
    gave_up: field_str(obj, "gave_up"),
    restarted_as: field_str(obj, "restarted_as"),
  }
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
  let previous_attempt = field_num(obj, "previous_attempt").map(|value| value as usize);
  let label = field_str(obj, "label").unwrap_or_default();
  let kind = ProcKind::parse(field_str(obj, "kind").as_deref().unwrap_or("skill")).unwrap_or(ProcKind::Skill);
  let status =
    ProcStatus::parse(field_str(obj, "status").as_deref().unwrap_or("waiting")).unwrap_or(ProcStatus::Waiting);
  let note = field_str(obj, "note");
  let detail = field_str(obj, "detail");
  let fail_reason = field_str(obj, "fail_reason");
  let elapsed = field_num(obj, "elapsed");
  let container_name = field_str(obj, "container_name");
  let container_runtime = field_str(obj, "container_runtime");
  let cast_path = field_str(obj, "cast_path");
  let transcript_path = field_str(obj, "transcript_path"); // absent on sessions persisted by older builds
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
    previous_attempt,
    order: field_order(obj), // absent on sessions persisted by older builds
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
    suspected_cause: field_str(obj, "suspected_cause").as_deref().and_then(crate::failure::SuspectedCause::parse),
    elapsed,
    container_name,
    container_runtime,
    cast_path,
    transcript_path,
    diff_path,
    skill_source,
    route,
    result_path,
    annotate_target: field_str(obj, "annotate_target"), // absent on sessions persisted by older builds
    // Also absent on older snapshots. A restored proc that WAS parked reads as plainly running
    // until its next report, which is the harmless direction: the phase is live decoration, and
    // the run either resumes and clears it or fails and replaces it.
    phase: field_str(obj, "phase"),
    phase_until: field_num(obj, "phase_until").map(|n| n as u64),
    recovery: obj
      .iter()
      .find(|(key, _)| key == "recovery")
      .and_then(|(_, value)| crate::recovery::Recovery::parse(value)),
    usage: match field_value(obj, "usage").ok() {
      None | Some(Value::Null) => None,
      Some(value) => {
        Some(crate::usage::Summary::from_json(&crate::json::write(value)).ok_or("invalid TokenUsage on proc")?)
      }
    },
    quota: match field_value(obj, "quota").ok() {
      None | Some(Value::Null) => None, // absent on sessions persisted by older builds
      Some(value) => Some(crate::quota::RunQuota::from_value(value).ok_or("invalid quota on proc")?),
    },
    lines,
  })
}

/// A task's report-order key (`"order"`); anything but an array of non-negative integers
/// reads as no key.
pub(crate) fn field_order(obj: &[(String, Value)]) -> Vec<u32> {
  let Ok(Value::Array(items)) = field_value(obj, "order") else { return Vec::new() };
  let key: Option<Vec<u32>> = items
    .iter()
    .map(|item| match item {
      Value::Number(n) if *n >= 0.0 && n.fract() == 0.0 && *n <= f64::from(u32::MAX) => Some(*n as u32),
      _ => None,
    })
    .collect();
  key.unwrap_or_default()
}

/// A report-order key as a JSON array.
fn order_json(order: &[u32]) -> String {
  format!("[{}]", order.iter().map(u32::to_string).collect::<Vec<_>>().join(", "))
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
  use crate::daemon::model::{ReportEntry, ReportSection};

  #[test]
  fn task_keys_persist_and_the_live_report_arrives_in_page_order() {
    let stored = r#"{ "id": "ord", "started_at": 1, "procs": [
      { "index": 0, "order": [1, 1], "label": "host: plan", "lines": [] },
      { "index": 1, "order": [26, 1], "label": "host: publish", "lines": [] },
      { "index": 2, "order": [1, -1], "label": "bad key", "lines": [] },
      { "index": 3, "label": "older record", "lines": [] }
    ], "report": [
      { "section": "results", "proc": 0, "source": "plan", "markdown": "planned" },
      { "section": "results", "proc": 1, "source": "publish", "markdown": "published" }
    ] }"#;
    let session = parse_session_json(stored).unwrap();
    let keys: Vec<&[u32]> = session.procs.iter().map(|p| p.order.as_slice()).collect();
    assert_eq!(keys, [&[1, 1][..], &[26, 1], &[], &[]], "a malformed or absent key reads as none");
    let again = session_json_store(&session);
    assert!(again.contains(r#""index": 1, "previous_attempt": null, "order": [26, 1],"#), "got: {again}");
    assert!(again.find("planned") < again.find("published"), "the store keeps arrival order");
    assert_eq!(parse_session_json(&again).unwrap().procs, session.procs);

    let live = archived_session_json_api(&session);
    let at = |needle: &str| live.find(needle).unwrap_or_else(|| panic!("missing {needle}: {live}"));
    assert!(at("published") < at("planned"), "the live view is already in page order");
    assert!(live.contains(r#""order": "26.1""#) && live.contains(r#""order": "1.1""#), "{live}");
  }

  #[test]
  fn report_entries_persist_as_markdown_and_reach_the_page_rendered() {
    let mut session = Session {
      id: "rep".into(),
      started_at: 1,
      ended_at: None,
      profile: None,
      kind: None,
      repo: "/r".into(),
      branch: String::new(),
      skills: Vec::new(),
      procs: Vec::new(),
      last_seen_at: 1,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
      report: Vec::new(),
    };
    session.push_report(ReportEntry {
      section: ReportSection::Results,
      proc: Some(3),
      source: "publish".into(),
      markdown: "## Done\n\n<b>not html</b>".into(),
    });
    let stored = session_json_store(&session);
    assert!(stored.contains(r###""report": [{ "section": "results", "proc": 3, "source": "publish", "markdown": "## Done\n\n<b>not html</b>" }]"###), "got: {stored}");
    assert!(!stored.contains("\"html\""), "the store keeps markdown only");
    let back = parse_session_json(&stored).unwrap();
    assert_eq!(back.report, session.report);
    let live = archived_session_json_api(&session);
    assert!(
      live.contains(r#""html": "<h2>Done</h2><p>&lt;b&gt;not html&lt;/b&gt;</p>""#),
      "rendered and escaped: {live}"
    );
    // A record from before the sections existed, and a junk entry, both load cleanly.
    let legacy = parse_session_json(r#"{ "id": "old", "started_at": 1, "procs": [], "report": [{ "section": "warnings", "markdown": "x" }, { "section": "log" }] }"#).unwrap();
    assert!(legacy.report.is_empty());
  }

  #[test]
  fn proc_json_serializes_non_finite_as_null() {
    let proc = ProcRecord {
      index: 0,
      previous_attempt: None,
      order: Vec::new(),
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
      suspected_cause: None,
      elapsed: Some(f64::NAN),
      lines: vec![OutputLine { at: f64::INFINITY, text: "x".into() }],
      container_name: None,
      container_runtime: None,
      cast_path: None,
      transcript_path: None,
      diff_path: None,
      skill_source: None,
      route: None,
      result_path: None,
      annotate_target: None,
      phase: None,
      phase_until: None,
      recovery: None,
      usage: None,
      quota: None,
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
        previous_attempt: Some(7),
        order: Vec::new(),
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
        suspected_cause: None,
        elapsed: Some(1.5),
        container_name: None,
        container_runtime: Some("container".into()),
        cast_path: Some("/tmp/scsh-daemon/casts/abcdef-p0.cast".into()),
        transcript_path: None,
        diff_path: Some("/tmp/scsh-home/sessions/abcdef/diffs/add.html".into()),
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
        phase: None,
        phase_until: None,
        recovery: None,
        lines: vec![OutputLine { at: 0.1, text: "step 1".into() }],
        usage: None,
        quota: None,
      }],
      last_seen_at: 105,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
      report: Vec::new(),
    };
    // The per-session JSON the store DB reads/writes must roundtrip every field.
    let s = parse_session_json(&session_json_store(&session)).unwrap();
    assert_eq!(s.procs[0].lines[0].text, "step 1");
    assert_eq!(s.procs[0].detail.as_deref(), Some("up to date"));
    assert_eq!(s.procs[0].cast_path.as_deref(), Some("/tmp/scsh-daemon/casts/abcdef-p0.cast"));
    assert_eq!(s.procs[0].diff_path.as_deref(), Some("/tmp/scsh-home/sessions/abcdef/diffs/add.html"));
    assert_eq!(s.procs[0].container_runtime.as_deref(), Some("container"));
    assert_eq!(s.procs[0].previous_attempt, Some(7));
    assert_eq!(s.ended_at, Some(105));
    assert_eq!(s.skills[0].name, "add");
    assert_eq!(s.parent_session, None);

    for cause in
      [crate::failure::SuspectedCause::ExpiredCredentials, crate::failure::SuspectedCause::ToolCallParseFailure]
    {
      session.procs[0].status = ProcStatus::Fail;
      session.procs[0].fail_reason = Some(crate::failure::reason::CONTAINER_INACTIVE.into());
      session.procs[0].suspected_cause = Some(cause);
      let saved = session_json_store(&session);
      let restored = parse_session_json(&saved).unwrap();
      assert_eq!(restored.procs[0].suspected_cause, Some(cause));
      assert_eq!(restored.procs[0].fail_reason.as_deref(), Some(crate::failure::reason::CONTAINER_INACTIVE));
      let legacy = saved.replace(&format!(", \"suspected_cause\": {}", quote(cause.as_str())), "");
      assert_eq!(parse_session_json(&legacy).unwrap().procs[0].suspected_cause, None);
    }

    let usage = crate::usage::Summary {
      harness: crate::usage::Harness::Codex,
      complete: true,
      tokens: Some(crate::usage::Tokens { input: 10, output: 2, cache_read: 30, cache_write: None }),
      llm_round_trips: None,
      tool_calls: None,
    };
    session.procs[0].usage = Some(usage.clone());
    let saved = session_json_store(&session);
    assert_eq!(parse_session_json(&saved).unwrap().procs[0].usage, Some(usage));
    assert!(parse_session_json(&saved.replace("\"schema_version\":1", "\"schema_version\":2")).is_err());

    session.parent_session = Some("parent1".into());
    let with_parent = parse_session_json(&session_json_store(&session)).unwrap();
    assert_eq!(with_parent.parent_session.as_deref(), Some("parent1"));
    assert!(session_json_store(&session).contains("\"parent_session\": \"parent1\""));

    // Default supervisor state stays OUT of the JSON (old daemons must still parse it),
    // and a non-default one roundtrips every field.
    assert!(!session_json_store(&session).contains("supervisor"));
    session.supervisor = crate::daemon::model::SupervisorState {
      job_attempt: 3,
      retries: 10,
      next_retry_at: Some(2000),
      fail_signature: Some("fix|harness_nonzero_exit".into()),
      fail_streak: 2,
      gave_up: None,
      restarted_as: Some("newone".into()),
    };
    let sup = parse_session_json(&session_json_store(&session)).unwrap().supervisor;
    assert_eq!(sup, session.supervisor);
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
      supervisor: Default::default(),
      report: Vec::new(),
    };
    let parsed = parse_session_json(&session_json_store(&session)).unwrap();
    assert_eq!(parsed.workflow.as_ref().unwrap().nodes.len(), 2);
    assert_eq!(parsed.workflow.as_ref().unwrap().nodes[1].needs, vec!["add".to_string()]);
    session.workflow = None;
    let legacy = parse_session_json(&session_json_store(&session)).unwrap();
    assert!(legacy.workflow.is_none());
    // Live API synthesizes a graph from skills/procs even without authored workflow.
    let live = archived_session_json_api(&session);
    assert!(!live.contains(", \"workflow\":"), "empty session has no effective graph: {live}");
    assert!(live.contains("\"lifecycle\":"), "live API carries the daemon-authoritative lifecycle: {live}");
    assert!(!session_json_store(&session).contains("\"lifecycle\":"), "derived lifecycle is not persisted");
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
  fn transcript_growth_json_shape_matches_ws_schema() {
    assert_eq!(
      transcript_growth_json("abcdef", 2, true),
      r#"{ "type": "transcript_growth", "session": "abcdef", "proc": 2, "running": true }"#
    );
    assert!(transcript_growth_json("a\"b", 0, false).contains(r#""session": "a\"b""#));
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
        supervisor: Default::default(),
        report: Vec::new(),
      },
    );
    let light = tick_json_light(&store, 105);
    assert!(!light.contains("\"sessions\""));
    let full = tick_json(&store, 105);
    assert!(full.contains("\"sessions\""));
  }
}
