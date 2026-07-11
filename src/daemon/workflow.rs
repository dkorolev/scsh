//! Workflow dependency-graph metadata for the session browser.
//!
//! Topology comes from a harness definition's `steps` / `needs` (never from proc notes).
//! Display state is derived from proc status plus session lifecycle (stalled ≠ silent).

use super::model::{ProcKind, ProcRecord, ProcStatus, Session, SessionLifecycle};
use crate::harness_def::HarnessDef;

/// Immutable DAG for one workflow session — optional on [`Session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMeta {
  pub nodes: Vec<WorkflowNodeMeta>,
}

/// One declared workflow step in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowNodeMeta {
  /// Declared step id (graph key).
  pub id: String,
  /// Matching skill proc index once registered; `None` until `proc/add`.
  pub proc_index: Option<usize>,
  /// Definition order (stable layout tie-breaker).
  pub order: usize,
  /// Direct `needs` edges (authoritative).
  pub needs: Vec<String>,
  /// True when the step has a `when:` gate.
  pub conditional: bool,
  /// Human-readable gate summary for the job-page marker (e.g. `Runs only if step.ok = true`).
  /// Absent on older session snapshots that only stored [`Self::conditional`].
  pub when_summary: Option<String>,
}

/// User-facing graph node state (includes derived `stalled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkflowDisplayState {
  Waiting,
  Ready,
  Running,
  Done,
  Failed,
  Skipped,
  Stalled,
}

impl WorkflowDisplayState {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Waiting => "waiting",
      Self::Ready => "ready",
      Self::Running => "running",
      Self::Done => "done",
      Self::Failed => "failed",
      Self::Skipped => "skipped",
      Self::Stalled => "stalled",
    }
  }

  pub fn label(self) -> &'static str {
    match self {
      Self::Waiting => "Waiting",
      Self::Ready => "Ready",
      Self::Running => "Running",
      Self::Done => "Done",
      Self::Failed => "Failed",
      Self::Skipped => "Skipped",
      Self::Stalled => "Stalled",
    }
  }
}

/// Build graph metadata from a parsed workflow definition. Flat defs → `None`.
pub fn workflow_meta_from_def(def: &HarnessDef) -> Option<WorkflowMeta> {
  if !def.is_workflow() {
    return None;
  }
  let meta = WorkflowMeta {
    nodes: def
      .steps
      .iter()
      .enumerate()
      .map(|(order, s)| WorkflowNodeMeta {
        id: s.id.clone(),
        proc_index: None,
        order,
        needs: s.needs.clone(),
        conditional: s.when.is_some(),
        when_summary: s.when.as_ref().map(crate::harness_def::format_when_summary),
      })
      .collect(),
  };
  validate_workflow_meta(&meta).ok()?;
  Some(meta)
}

/// Validate topology. On failure the caller should omit the graph, not reject the session.
pub fn validate_workflow_meta(meta: &WorkflowMeta) -> Result<(), String> {
  if meta.nodes.is_empty() {
    return Err("empty workflow graph".into());
  }
  let mut seen = std::collections::BTreeSet::new();
  for n in &meta.nodes {
    if n.id.is_empty() || !n.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
      return Err(format!("unsafe step id {:?}", n.id));
    }
    if !seen.insert(n.id.as_str()) {
      return Err(format!("duplicate step id {}", n.id));
    }
  }
  let ids: std::collections::BTreeSet<&str> = meta.nodes.iter().map(|n| n.id.as_str()).collect();
  for n in &meta.nodes {
    let mut need_seen = std::collections::BTreeSet::new();
    for need in &n.needs {
      if need == &n.id {
        return Err(format!("self-edge on {}", n.id));
      }
      if !ids.contains(need.as_str()) {
        return Err(format!("unknown need {need} on {}", n.id));
      }
      if !need_seen.insert(need.as_str()) {
        return Err(format!("duplicate need {need} on {}", n.id));
      }
    }
  }
  if let Some(cycle) = find_cycle(meta) {
    return Err(format!("cycle involving {cycle}"));
  }
  // proc_index uniqueness among set indices
  let mut procs = std::collections::BTreeSet::new();
  for n in &meta.nodes {
    if let Some(i) = n.proc_index {
      if !procs.insert(i) {
        return Err(format!("duplicate proc_index {i}"));
      }
    }
  }
  Ok(())
}

fn find_cycle(meta: &WorkflowMeta) -> Option<String> {
  use std::collections::BTreeMap;
  let by_id: BTreeMap<&str, &WorkflowNodeMeta> = meta.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
  let mut state: BTreeMap<&str, u8> = BTreeMap::new(); // 0=unseen 1=stack 2=done
  fn dfs<'a>(
    id: &'a str, by_id: &BTreeMap<&str, &'a WorkflowNodeMeta>, state: &mut BTreeMap<&'a str, u8>,
  ) -> Option<&'a str> {
    state.insert(id, 1);
    let node = by_id.get(id)?;
    for need in &node.needs {
      match state.get(need.as_str()).copied().unwrap_or(0) {
        1 => return Some(need.as_str()),
        2 => {}
        _ => {
          if let Some(c) = dfs(need, by_id, state) {
            return Some(c);
          }
        }
      }
    }
    state.insert(id, 2);
    None
  }
  for n in &meta.nodes {
    if state.get(n.id.as_str()).copied().unwrap_or(0) == 0 {
      if let Some(c) = dfs(&n.id, &by_id, &mut state) {
        return Some(c.to_string());
      }
    }
  }
  None
}

/// Bind a skill proc to its workflow node by step id. Ignores builds and unknown ids.
pub fn bind_workflow_proc(meta: &mut WorkflowMeta, step_id: &str, proc_index: usize, kind: ProcKind) {
  if kind != ProcKind::Skill || step_id.is_empty() {
    return;
  }
  if let Some(node) = meta.nodes.iter_mut().find(|n| n.id == step_id) {
    node.proc_index = Some(proc_index);
  }
}

/// Parse optional workflow JSON object; invalid → `None` (omit graph).
pub fn parse_workflow_value(v: Option<&crate::json::Value>) -> Option<WorkflowMeta> {
  let v = v?;
  let crate::json::Value::Object(obj) = v else {
    return None;
  };
  let nodes_v = obj.iter().find(|(k, _)| k == "nodes").map(|(_, v)| v)?;
  let crate::json::Value::Array(arr) = nodes_v else {
    return None;
  };
  let mut nodes = Vec::new();
  for item in arr {
    let crate::json::Value::Object(nobj) = item else {
      return None;
    };
    let id = super::jsonio::field_str(nobj, "id")?;
    let order = super::jsonio::field_num(nobj, "order").unwrap_or(0.0) as usize;
    let proc_index = super::jsonio::field_num(nobj, "proc_index").map(|n| n as usize);
    let conditional = match nobj.iter().find(|(k, _)| k == "conditional").map(|(_, v)| v) {
      Some(crate::json::Value::Bool(b)) => *b,
      _ => false,
    };
    let when_summary = match nobj.iter().find(|(k, _)| k == "when_summary").map(|(_, v)| v) {
      Some(crate::json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
      _ => None,
    };
    let needs = match nobj.iter().find(|(k, _)| k == "needs").map(|(_, v)| v) {
      Some(crate::json::Value::Array(a)) => a
        .iter()
        .filter_map(|x| match x {
          crate::json::Value::String(s) => Some(s.clone()),
          _ => None,
        })
        .collect(),
      _ => Vec::new(),
    };
    nodes.push(WorkflowNodeMeta { id, proc_index, order, needs, conditional, when_summary });
  }
  let meta = WorkflowMeta { nodes };
  validate_workflow_meta(&meta).ok()?;
  Some(meta)
}

pub fn workflow_json(meta: &WorkflowMeta) -> String {
  let nodes: Vec<String> = meta
    .nodes
    .iter()
    .map(|n| {
      let needs: Vec<String> = n.needs.iter().map(|s| crate::json::quote(s)).collect();
      let proc = match n.proc_index {
        Some(i) => format!("{i}"),
        None => "null".into(),
      };
      let when_summary = match &n.when_summary {
        Some(s) => crate::json::quote(s),
        None => "null".into(),
      };
      format!(
        "{{ \"id\": {}, \"proc_index\": {proc}, \"order\": {}, \"needs\": [{}], \"conditional\": {}, \"when_summary\": {when_summary} }}",
        crate::json::quote(&n.id),
        n.order,
        needs.join(", "),
        if n.conditional { "true" } else { "false" },
      )
    })
    .collect();
  format!("{{ \"nodes\": [{}] }}", nodes.join(", "))
}

fn proc_by_index<'a>(session: &'a Session, index: usize) -> Option<&'a ProcRecord> {
  session.procs.iter().find(|p| p.index == index)
}

fn node_proc<'a>(session: &'a Session, node: &WorkflowNodeMeta) -> Option<&'a ProcRecord> {
  node.proc_index.and_then(|i| proc_by_index(session, i))
}

fn status_terminal(status: ProcStatus) -> bool {
  matches!(status, ProcStatus::Ok | ProcStatus::Fail | ProcStatus::Skipped)
}

/// Unmet direct prerequisites (non-terminal or missing procs).
pub fn unmet_needs(session: &Session, node: &WorkflowNodeMeta) -> usize {
  let Some(meta) = session.workflow.as_ref() else {
    return node.needs.len();
  };
  node
    .needs
    .iter()
    .filter(|need| {
      let Some(n) = meta.nodes.iter().find(|x| x.id == **need) else {
        return true;
      };
      match node_proc(session, n) {
        Some(p) => !status_terminal(p.status),
        None => true,
      }
    })
    .count()
}

/// Shared display-state derivation for SSR and live JS.
pub fn display_state(session: &Session, node: &WorkflowNodeMeta, now: u64) -> WorkflowDisplayState {
  let life = session.lifecycle_status(now);
  let stalled_session = life == SessionLifecycle::Terminated;
  match node_proc(session, node) {
    None => {
      if stalled_session {
        WorkflowDisplayState::Stalled
      } else {
        WorkflowDisplayState::Waiting
      }
    }
    Some(p) => match p.status {
      ProcStatus::Ok => WorkflowDisplayState::Done,
      ProcStatus::Fail => WorkflowDisplayState::Failed,
      ProcStatus::Skipped => WorkflowDisplayState::Skipped,
      ProcStatus::Running => {
        if stalled_session {
          WorkflowDisplayState::Stalled
        } else {
          WorkflowDisplayState::Running
        }
      }
      ProcStatus::Waiting => {
        if stalled_session {
          WorkflowDisplayState::Stalled
        } else if unmet_needs(session, node) == 0 {
          WorkflowDisplayState::Ready
        } else {
          WorkflowDisplayState::Waiting
        }
      }
    },
  }
}

/// Topological rank: roots = 0, else 1 + max(rank(need)).
pub fn node_ranks(meta: &WorkflowMeta) -> Vec<usize> {
  use std::collections::BTreeMap;
  let by_id: BTreeMap<&str, &WorkflowNodeMeta> = meta.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
  let mut ranks: BTreeMap<&str, usize> = BTreeMap::new();
  fn rank_of<'a>(
    id: &'a str, by_id: &BTreeMap<&str, &'a WorkflowNodeMeta>, ranks: &mut BTreeMap<&'a str, usize>,
  ) -> usize {
    if let Some(&r) = ranks.get(id) {
      return r;
    }
    let node = match by_id.get(id) {
      Some(n) => *n,
      None => {
        ranks.insert(id, 0);
        return 0;
      }
    };
    let r = if node.needs.is_empty() {
      0
    } else {
      1 + node.needs.iter().map(|n| rank_of(n, by_id, ranks)).max().unwrap_or(0)
    };
    ranks.insert(id, r);
    r
  }
  meta.nodes.iter().map(|n| rank_of(&n.id, &by_id, &mut ranks)).collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::daemon::model::{DaemonMode, Store};

  fn arith_meta() -> WorkflowMeta {
    WorkflowMeta {
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
          id: "multiply".into(),
          proc_index: Some(1),
          order: 1,
          needs: vec![],
          conditional: false,
          when_summary: None,
        },
        WorkflowNodeMeta {
          id: "summarize".into(),
          proc_index: Some(2),
          order: 2,
          needs: vec!["add".into(), "multiply".into()],
          conditional: false,
          when_summary: None,
        },
      ],
    }
  }

  #[test]
  fn validates_arith_and_rejects_cycles() {
    assert!(validate_workflow_meta(&arith_meta()).is_ok());
    let mut bad = arith_meta();
    bad.nodes[0].needs.push("summarize".into());
    assert!(validate_workflow_meta(&bad).is_err());
  }

  #[test]
  fn ranks_fan_in() {
    let ranks = node_ranks(&arith_meta());
    assert_eq!(ranks, vec![0, 0, 1]);
  }

  #[test]
  fn stalled_when_session_terminated() {
    let mut store = Store::new(DaemonMode::Persistent, 7274, 100);
    let mut session = Session {
      id: "abcdef".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("arith".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![ProcRecord {
        index: 0,
        label: "claude: add".into(),
        kind: ProcKind::Skill,
        status: ProcStatus::Running,
        skill_name: Some("add".into()),
        harness: Some("claude".into()),
        model: None,
        started_at: Some(90),
        note: None,
        detail: None,
        fail_reason: None,
        elapsed: None,
        lines: vec![],
        container_name: None,
        cast_path: None,
        diff_path: None,
        skill_source: Some("add".into()),
        route: None,
        result_path: None,
      }],
      last_seen_at: 80, // stale vs now=100 with SESSION_STALE_SECS=10
      client_connected: true,
      run_pid: Some(1),
      workflow: Some(arith_meta()),
    };
    // Only first node mapped for this test
    session.workflow.as_mut().unwrap().nodes[1].proc_index = None;
    session.workflow.as_mut().unwrap().nodes[2].proc_index = None;
    store.sessions.insert("abcdef".into(), session.clone());
    let node = &session.workflow.as_ref().unwrap().nodes[0];
    assert_eq!(display_state(&session, node, 100), WorkflowDisplayState::Stalled);
  }

  #[test]
  fn builtins_yield_valid_workflow_meta() {
    for name in ["arith", "fruits", "code-review", "greet"] {
      let (_, src) = crate::harness_def::builtin_defs().into_iter().find(|(n, _)| *n == name).unwrap();
      let def = crate::harness_def::validate(name, src, crate::harness_def::DefSource::Builtin)
        .unwrap_or_else(|e| panic!("{name}: {}", e.join("; ")));
      let meta = workflow_meta_from_def(&def).expect(name);
      assert!(validate_workflow_meta(&meta).is_ok(), "{name}");
    }
    let (_, src) = crate::harness_def::builtin_defs().into_iter().find(|(n, _)| *n == "code-review").unwrap();
    let def = crate::harness_def::validate("code-review", src, crate::harness_def::DefSource::Builtin).unwrap();
    let meta = workflow_meta_from_def(&def).unwrap();
    let review = meta.nodes.iter().find(|n| n.id == "review").unwrap();
    assert!(review.conditional);
    assert_eq!(review.when_summary.as_deref(), Some("Runs only if probe_credentials.ok = true"));
    let (_, src) = crate::harness_def::builtin_defs().into_iter().find(|(n, _)| *n == "add").unwrap();
    let add = crate::harness_def::validate("add", src, crate::harness_def::DefSource::Builtin).unwrap();
    assert!(workflow_meta_from_def(&add).is_none());
  }
}
