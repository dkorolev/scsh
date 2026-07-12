//! Workflow / job dependency-graph metadata for the session browser.
//!
//! Authored topology comes from a harness definition's `steps` / `needs` (stored on the
//! session). The UI always renders [`effective_workflow_meta`], which merges that DAG with
//! live image-build procs (`build_base` → `build_{harness}` → skills) so every job — flat
//! definition, profile, workflow, or build-images — gets a dependency graph.

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

/// User-facing graph node state (includes derived `stalled` / `force-stopped`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkflowDisplayState {
  Waiting,
  Ready,
  Running,
  Done,
  Failed,
  /// Failed because the user (or session Force stop) killed it — not a natural failure.
  ForceStopped,
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
      Self::ForceStopped => "force-stopped",
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
      Self::ForceStopped => "Force-stopped",
      Self::Skipped => "Skipped",
      Self::Stalled => "Abandoned",
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
        id: if s.repeat.is_some() {
          format!("{}__repeat", s.id)
        } else if s.do_while.is_some() {
          format!("{}__while", s.id)
        } else {
          s.id.clone()
        },
        proc_index: None,
        order,
        needs: s.needs.clone(),
        conditional: s.when.is_some(),
        when_summary: None, // never persist gate literals (REMAINS-TO-DO §3)
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
    if n.id.is_empty() || !n.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
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

/// The dynamic-loop node-id suffixes, shared with the orchestrator's
/// [`crate::harness_def::Step::iteration_run_id`]: `<step>__repeat` / `<step>__while` is the
/// authored template node, and iterations arrive as `<step>__repeat_<n>` / `<step>__while_<n>`.
pub const LOOP_SUFFIXES: [&str; 2] = ["__repeat", "__while"];

/// Split a dynamic loop-iteration id like `increment__while_2` into `(base, suffix, iteration)`
/// — here `("increment", "__while", 2)`. `None` for ordinary step ids.
pub fn parse_loop_iteration_id(id: &str) -> Option<(&str, &str, usize)> {
  for suffix in LOOP_SUFFIXES {
    let marker = format!("{suffix}_");
    if let Some((base, n)) = id.rsplit_once(marker.as_str()) {
      if let Ok(iteration) = n.parse::<usize>() {
        return Some((base, suffix, iteration));
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
    return;
  }
  if let Some((base, suffix, iteration)) = parse_loop_iteration_id(step_id) {
    if iteration == 0 {
      return;
    }
    let template_id = format!("{base}{suffix}");
    let Some(template) = meta.nodes.iter().find(|n| n.id == template_id).cloned() else { return };
    let previous = format!("{base}{suffix}_{}", iteration - 1);
    let needs = if iteration == 1 { template.needs } else { vec![previous] };
    meta.nodes.push(WorkflowNodeMeta {
      id: step_id.to_string(),
      proc_index: Some(proc_index),
      order: template.order + iteration,
      needs,
      conditional: false,
      when_summary: None,
    });
  }
}

/// Graph the session browser shows: authored workflow DAG (if any) plus image-build nodes
/// and edges into the skills that need those images. Flat / profile / build-only jobs get a
/// synthesized skill matrix. Returns `None` only when there is nothing to draw.
pub fn effective_workflow_meta(session: &Session) -> Option<WorkflowMeta> {
  let mut nodes: Vec<WorkflowNodeMeta> = Vec::new();
  let mut order: usize = 0;

  // --- Image builds that actually ran (cache hits omit the proc → omit the node) ---
  let mut base_proc: Option<usize> = None;
  let mut harness_procs: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
  for p in &session.procs {
    if p.kind != ProcKind::Build {
      continue;
    }
    match p.harness.as_deref() {
      None => base_proc = Some(p.index),
      Some(h) if is_safe_graph_id(h) => {
        harness_procs.insert(h.to_string(), p.index);
      }
      Some(_) => {}
    }
  }
  if let Some(idx) = base_proc {
    nodes.push(WorkflowNodeMeta {
      id: "build_base".into(),
      proc_index: Some(idx),
      order,
      needs: vec![],
      conditional: false,
      when_summary: None,
    });
    order += 1;
  }
  let build_id_for: std::collections::BTreeMap<String, String> =
    harness_procs.keys().map(|h| (h.clone(), format!("build_{h}"))).collect();
  for (h, idx) in &harness_procs {
    let mut needs = Vec::new();
    if base_proc.is_some() {
      needs.push("build_base".into());
    }
    nodes.push(WorkflowNodeMeta {
      id: format!("build_{h}"),
      proc_index: Some(*idx),
      order,
      needs,
      conditional: false,
      when_summary: None,
    });
    order += 1;
  }

  // --- Skill / step nodes ---
  let def_needs = needs_from_harness_profile(session);
  if let Some(authored) = &session.workflow {
    for n in &authored.nodes {
      if !is_safe_graph_id(&n.id) {
        continue;
      }
      // A loop template (repeat / do-while) is orchestration metadata, not a task. Iteration
      // nodes are appended to the authored graph by `bind_workflow_proc` only as they start.
      if LOOP_SUFFIXES.iter().any(|suffix| n.id.ends_with(suffix)) {
        continue;
      }
      let mut needs = n.needs.clone();
      if needs.is_empty() {
        if let Some(map) = &def_needs {
          if let Some(dn) = map.get(&n.id) {
            needs = dn.clone();
          }
        }
      }
      if let Some(h) = harness_for_step(session, &n.id) {
        if let Some(bid) = build_id_for.get(h) {
          if !needs.iter().any(|x| x == bid) {
            needs.push(bid.clone());
          }
        }
      }
      let proc_index = latest_skill_proc_index(session, &n.id).or(n.proc_index);
      nodes.push(WorkflowNodeMeta {
        id: n.id.clone(),
        proc_index,
        order: order + n.order,
        needs,
        conditional: n.conditional,
        when_summary: n.when_summary.clone(),
      });
    }
  } else {
    // Flat definition / profile / build-images: one node per skill (from procs, else planned).
    let mut seen = std::collections::BTreeSet::new();
    // Latest proc wins for retries.
    let mut by_name: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for p in &session.procs {
      if p.kind != ProcKind::Skill {
        continue;
      }
      let Some(name) = p.skill_name.as_deref() else {
        continue;
      };
      if !is_safe_graph_id(name) {
        continue;
      }
      by_name.insert(name.to_string(), p.index);
    }
    for (name, idx) in &by_name {
      seen.insert(name.clone());
      let mut needs = def_needs.as_ref().and_then(|m| m.get(name)).cloned().unwrap_or_default();
      if let Some(h) = session.procs.iter().find(|p| p.index == *idx).and_then(|p| p.harness.as_deref()) {
        if let Some(bid) = build_id_for.get(h) {
          if !needs.iter().any(|x| x == bid) {
            needs.push(bid.clone());
          }
        }
      } else if let Some(sk) = session.skills.iter().find(|s| s.name == *name) {
        if let Some(bid) = build_id_for.get(&sk.harness) {
          if !needs.iter().any(|x| x == bid) {
            needs.push(bid.clone());
          }
        }
      }
      nodes.push(WorkflowNodeMeta {
        id: name.clone(),
        proc_index: Some(*idx),
        order,
        needs,
        conditional: false,
        when_summary: None,
      });
      order += 1;
    }
    // Planned skills not yet registered as procs (browser pre-create).
    for sk in &session.skills {
      if !seen.insert(sk.name.clone()) {
        continue;
      }
      if !is_safe_graph_id(&sk.name) {
        continue;
      }
      let mut needs = def_needs.as_ref().and_then(|m| m.get(&sk.name)).cloned().unwrap_or_default();
      if let Some(bid) = build_id_for.get(&sk.harness) {
        if !needs.iter().any(|x| x == bid) {
          needs.push(bid.clone());
        }
      }
      nodes.push(WorkflowNodeMeta {
        id: sk.name.clone(),
        proc_index: None,
        order,
        needs,
        conditional: false,
        when_summary: None,
      });
      order += 1;
    }
  }

  if nodes.is_empty() {
    return None;
  }
  let meta = WorkflowMeta { nodes };
  validate_workflow_meta(&meta).ok()?;
  Some(meta)
}

fn is_safe_graph_id(id: &str) -> bool {
  !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Step `needs` from the session's harness definition — repairs legacy sessions that were
/// persisted before dependency edges were stored on `session.workflow`.
#[cfg(test)]
pub(crate) fn needs_from_harness_profile_for_test(
  session: &Session,
) -> Option<std::collections::BTreeMap<String, Vec<String>>> {
  needs_from_harness_profile(session)
}

fn needs_from_harness_profile(session: &Session) -> Option<std::collections::BTreeMap<String, Vec<String>>> {
  if session.kind.as_deref() != Some("workflow") {
    return None;
  }
  let profile = session.profile.as_deref()?;
  let root = std::path::Path::new(&session.repo);
  let discovery = crate::harness_def::discover(root);
  let def = discovery.find(profile)?;
  if !def.is_workflow() {
    return None;
  }
  Some(def.steps.iter().map(|s| (s.id.clone(), s.needs.clone())).collect())
}

fn latest_skill_proc_index(session: &Session, skill_name: &str) -> Option<usize> {
  session
    .procs
    .iter()
    .filter(|p| p.kind == ProcKind::Skill && p.skill_name.as_deref() == Some(skill_name))
    .map(|p| p.index)
    .max()
}

fn harness_for_step<'a>(session: &'a Session, step_id: &str) -> Option<&'a str> {
  if let Some(p) = session
    .procs
    .iter()
    .filter(|p| p.kind == ProcKind::Skill && p.skill_name.as_deref() == Some(step_id))
    .max_by_key(|p| p.index)
  {
    return p.harness.as_deref();
  }
  session.skills.iter().find(|s| s.name == step_id).map(|s| s.harness.as_str())
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
    // Reject non-finite / negative / fractional indices rather than silently truncating.
    let order = match nobj.iter().find(|(k, _)| k == "order").map(|(_, v)| v) {
      None => 0usize,
      Some(crate::json::Value::Number(n))
        if n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= usize::MAX as f64 =>
      {
        *n as usize
      }
      Some(_) => return None,
    };
    let proc_index = match nobj.iter().find(|(k, _)| k == "proc_index").map(|(_, v)| v) {
      None | Some(crate::json::Value::Null) => None,
      Some(crate::json::Value::Number(n))
        if n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= usize::MAX as f64 =>
      {
        Some(*n as usize)
      }
      Some(_) => return None,
    };
    let conditional = match nobj.iter().find(|(k, _)| k == "conditional").map(|(_, v)| v) {
      Some(crate::json::Value::Bool(b)) => *b,
      Some(_) => return None,
      None => false,
    };
    // Legacy sessions may carry when_summary; accept but never re-emit (privacy §3).
    let _legacy_when_summary = nobj.iter().find(|(k, _)| k == "when_summary");
    let needs = match nobj.iter().find(|(k, _)| k == "needs").map(|(_, v)| v) {
      None => Vec::new(),
      Some(crate::json::Value::Array(a)) => {
        let mut out = Vec::new();
        for x in a {
          match x {
            crate::json::Value::String(s) => out.push(s.clone()),
            _ => return None, // strict: mixed-type needs arrays are invalid
          }
        }
        out
      }
      Some(_) => return None,
    };
    nodes.push(WorkflowNodeMeta { id, proc_index, order, needs, conditional, when_summary: None });
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
      // Do not emit when_summary — gate literals must not become durable browser metadata.
      format!(
        "{{ \"id\": {}, \"proc_index\": {proc}, \"order\": {}, \"needs\": [{}], \"conditional\": {} }}",
        crate::json::quote(&n.id),
        n.order,
        needs.join(", "),
        if n.conditional { "true" } else { "false" },
      )
    })
    .collect();
  format!("{{ \"nodes\": [{}] }}", nodes.join(", "))
}

fn proc_by_index(session: &Session, index: usize) -> Option<&ProcRecord> {
  session.procs.iter().find(|p| p.index == index)
}

fn node_proc<'a>(session: &'a Session, node: &WorkflowNodeMeta) -> Option<&'a ProcRecord> {
  node.proc_index.and_then(|i| proc_by_index(session, i))
}

fn status_terminal(status: ProcStatus) -> bool {
  matches!(status, ProcStatus::Ok | ProcStatus::Fail | ProcStatus::Skipped)
}

/// Unmet direct prerequisites (non-terminal or missing procs), resolved against `meta`
/// (typically [`effective_workflow_meta`]).
pub fn unmet_needs(session: &Session, meta: &WorkflowMeta, node: &WorkflowNodeMeta) -> usize {
  unmet_need_ids(session, meta, node).len()
}

/// Ids of direct prerequisites that are not yet terminal (or are missing).
pub fn unmet_need_ids<'a>(session: &Session, meta: &'a WorkflowMeta, node: &'a WorkflowNodeMeta) -> Vec<&'a str> {
  node
    .needs
    .iter()
    .filter_map(|need| {
      let Some(n) = meta.nodes.iter().find(|x| x.id == *need) else {
        return Some(need.as_str());
      };
      match node_proc(session, n) {
        Some(p) if status_terminal(p.status) => None,
        _ => Some(n.id.as_str()),
      }
    })
    .collect()
}

/// Shared display-state derivation for SSR and live JS.
pub fn display_state(
  session: &Session, meta: &WorkflowMeta, node: &WorkflowNodeMeta, now: u64,
) -> WorkflowDisplayState {
  let life = session.lifecycle_status(now);
  // Ready / Running are live-only. A cancelled, failed, completed, or abruptly terminated job
  // must not keep advertising "ready — not started yet" for waiting steps (that reads as the
  // next task still being about to launch).
  let live = life == SessionLifecycle::Running;
  match node_proc(session, node) {
    None => {
      if live {
        WorkflowDisplayState::Waiting
      } else {
        WorkflowDisplayState::Stalled
      }
    }
    Some(p) => match p.status {
      ProcStatus::Ok => WorkflowDisplayState::Done,
      ProcStatus::Fail => {
        if p.fail_reason.as_deref() == Some(crate::failure::reason::FORCE_STOPPED) {
          WorkflowDisplayState::ForceStopped
        } else {
          WorkflowDisplayState::Failed
        }
      }
      ProcStatus::Skipped => WorkflowDisplayState::Skipped,
      ProcStatus::Running => {
        if live {
          WorkflowDisplayState::Running
        } else {
          WorkflowDisplayState::Stalled
        }
      }
      ProcStatus::Waiting => {
        if !live {
          WorkflowDisplayState::Stalled
        } else if unmet_needs(session, meta, node) == 0 {
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
        annotate_target: None,
      }],
      last_seen_at: 60, // stale vs now=100 with SESSION_STALE_SECS=30
      client_connected: true,
      run_pid: Some(1),
      workflow: Some(arith_meta()),
      parent_session: None,
    };
    // Only first node mapped for this test
    session.workflow.as_mut().unwrap().nodes[1].proc_index = None;
    session.workflow.as_mut().unwrap().nodes[2].proc_index = None;
    store.sessions.insert("abcdef".into(), session.clone());
    let meta = session.workflow.as_ref().unwrap();
    let node = &meta.nodes[0];
    assert_eq!(display_state(&session, meta, node, 100), WorkflowDisplayState::Stalled);
  }

  #[test]
  fn waiting_skill_is_stalled_not_ready_when_job_ended_incomplete() {
    // Build finished; skill never started; session ended mid-job (daemon restart / orphan
    // reconcile). Lifecycle is Cancelled — must not keep advertising Ready.
    let session = Session {
      id: "cancel".into(),
      started_at: 1,
      ended_at: Some(50),
      profile: Some("smoke".into()),
      kind: Some("definition".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![
        ProcRecord {
          index: 0,
          label: "build grok".into(),
          kind: ProcKind::Build,
          status: ProcStatus::Ok,
          skill_name: None,
          harness: Some("grok".into()),
          model: None,
          started_at: Some(1),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(10.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 1,
          label: "grok: smoke".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Waiting,
          skill_name: Some("smoke".into()),
          harness: Some("grok".into()),
          model: None,
          started_at: None,
          note: Some("waiting for image build…".into()),
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("smoke".into()),
          route: Some("run".into()),
          result_path: None,
          annotate_target: None,
        },
      ],
      last_seen_at: 50,
      client_connected: false,
      run_pid: None,
      workflow: Some(WorkflowMeta {
        nodes: vec![
          WorkflowNodeMeta {
            id: "build_grok".into(),
            proc_index: Some(0),
            order: 0,
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
          WorkflowNodeMeta {
            id: "smoke".into(),
            proc_index: Some(1),
            order: 1,
            needs: vec!["build_grok".into()],
            conditional: false,
            when_summary: None,
          },
        ],
      }),
      parent_session: None,
    };
    assert_eq!(session.lifecycle_status(60), SessionLifecycle::Cancelled);
    let meta = session.workflow.as_ref().unwrap();
    assert_eq!(display_state(&session, meta, &meta.nodes[1], 60), WorkflowDisplayState::Stalled);
  }

  #[test]
  fn effective_meta_adds_build_nodes_for_flat_jobs() {
    let session = Session {
      id: "flat01".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("demo-pr".into()),
      kind: Some("definition".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![
        crate::daemon::model::SkillMeta { name: "demo-pr-claude-sonnet".into(), harness: "claude".into() },
        crate::daemon::model::SkillMeta { name: "demo-pr-cursor-composer-fast".into(), harness: "cursor".into() },
      ],
      procs: vec![
        ProcRecord {
          index: 0,
          label: "using Apple Containers · build base".into(),
          kind: ProcKind::Build,
          status: ProcStatus::Running,
          skill_name: None,
          harness: None,
          model: None,
          started_at: Some(1),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 1,
          label: "using Apple Containers · build claude".into(),
          kind: ProcKind::Build,
          status: ProcStatus::Waiting,
          skill_name: None,
          harness: Some("claude".into()),
          model: None,
          started_at: None,
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 2,
          label: "using Apple Containers · build cursor".into(),
          kind: ProcKind::Build,
          status: ProcStatus::Waiting,
          skill_name: None,
          harness: Some("cursor".into()),
          model: None,
          started_at: None,
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 3,
          label: "claude: demo-pr-claude-sonnet".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Waiting,
          skill_name: Some("demo-pr-claude-sonnet".into()),
          harness: Some("claude".into()),
          model: Some("sonnet".into()),
          started_at: None,
          note: Some("waiting for image build…".into()),
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("demo-pr".into()),
          route: Some("claude-sonnet".into()),
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 4,
          label: "cursor: demo-pr-cursor-composer-fast".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Waiting,
          skill_name: Some("demo-pr-cursor-composer-fast".into()),
          harness: Some("cursor".into()),
          model: Some("composer-2.5-fast".into()),
          started_at: None,
          note: Some("waiting for image build…".into()),
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("demo-pr".into()),
          route: Some("cursor-composer-fast".into()),
          result_path: None,
          annotate_target: None,
        },
      ],
      last_seen_at: 1,
      client_connected: true,
      run_pid: Some(1),
      workflow: None,
      parent_session: None,
    };
    let meta = effective_workflow_meta(&session).expect("flat job gets a graph");
    let ids: Vec<&str> = meta.nodes.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&"build_base"), "{ids:?}");
    assert!(ids.contains(&"build_claude"), "{ids:?}");
    assert!(ids.contains(&"build_cursor"), "{ids:?}");
    assert!(ids.contains(&"demo-pr-claude-sonnet"), "{ids:?}");
    let claude_skill = meta.nodes.iter().find(|n| n.id == "demo-pr-claude-sonnet").unwrap();
    assert_eq!(claude_skill.needs, vec!["build_claude".to_string()]);
    let build_claude = meta.nodes.iter().find(|n| n.id == "build_claude").unwrap();
    assert_eq!(build_claude.needs, vec!["build_base".to_string()]);
  }

  #[test]
  fn effective_meta_keeps_workflow_needs_and_adds_builds() {
    let mut session = Session {
      id: "arith1".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("arith".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![
        ProcRecord {
          index: 0,
          label: "using Apple Containers · build claude".into(),
          kind: ProcKind::Build,
          status: ProcStatus::Ok,
          skill_name: None,
          harness: Some("claude".into()),
          model: None,
          started_at: Some(1),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(5.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: None,
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 1,
          label: "claude: add".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: Some("add".into()),
          harness: Some("claude".into()),
          model: Some("sonnet".into()),
          started_at: Some(2),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(1.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("add".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 2,
          label: "codex: multiply".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: Some("multiply".into()),
          harness: Some("codex".into()),
          model: None,
          started_at: Some(2),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(1.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("multiply".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 3,
          label: "grok: summarize".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Waiting,
          skill_name: Some("summarize".into()),
          harness: Some("grok".into()),
          model: None,
          started_at: None,
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: None,
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("summarize".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
      ],
      last_seen_at: 1,
      client_connected: true,
      run_pid: Some(1),
      workflow: Some(arith_meta()),
      parent_session: None,
    };
    // Rebind authored proc indices to match this fixture.
    session.workflow.as_mut().unwrap().nodes[0].proc_index = Some(1);
    session.workflow.as_mut().unwrap().nodes[1].proc_index = Some(2);
    session.workflow.as_mut().unwrap().nodes[2].proc_index = Some(3);
    let meta = effective_workflow_meta(&session).unwrap();
    assert!(meta.nodes.iter().any(|n| n.id == "build_claude"));
    let add = meta.nodes.iter().find(|n| n.id == "add").unwrap();
    assert!(add.needs.contains(&"build_claude".to_string()), "{:?}", add.needs);
    let summarize = meta.nodes.iter().find(|n| n.id == "summarize").unwrap();
    assert!(summarize.needs.contains(&"add".to_string()));
    assert!(summarize.needs.contains(&"multiply".to_string()));
  }

  #[test]
  fn effective_meta_backfills_needs_from_profile_when_legacy_workflow_lost_edges() {
    let session = Session {
      id: "legacy".into(),
      started_at: 1,
      ended_at: Some(2),
      profile: Some("arith".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![
        ProcRecord {
          index: 0,
          label: "claude: add".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: Some("add".into()),
          harness: Some("claude".into()),
          model: None,
          started_at: Some(1),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(1.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("add".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 1,
          label: "codex: multiply".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: Some("multiply".into()),
          harness: Some("codex".into()),
          model: None,
          started_at: Some(1),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(1.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("multiply".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 2,
          label: "grok: summarize".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: Some("summarize".into()),
          harness: Some("grok".into()),
          model: None,
          started_at: Some(2),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(1.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("summarize".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
      ],
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
            needs: vec![],
            conditional: false,
            when_summary: None,
          },
        ],
      }),
      parent_session: None,
    };
    let meta = effective_workflow_meta(&session).unwrap();
    let summarize = meta.nodes.iter().find(|n| n.id == "summarize").unwrap();
    assert_eq!(summarize.needs, vec!["add".to_string(), "multiply".to_string()]);
  }

  #[test]
  fn effective_meta_backfills_needs_when_workflow_was_never_persisted() {
    let session = Session {
      id: "legacy-flat".into(),
      started_at: 1,
      ended_at: Some(2),
      profile: Some("arith".into()),
      kind: Some("workflow".into()),
      repo: "/tmp/r".into(),
      branch: "main".into(),
      skills: vec![
        crate::daemon::model::SkillMeta { name: "add".into(), harness: "claude".into() },
        crate::daemon::model::SkillMeta { name: "multiply".into(), harness: "codex".into() },
        crate::daemon::model::SkillMeta { name: "summarize".into(), harness: "grok".into() },
      ],
      procs: vec![
        ProcRecord {
          index: 0,
          label: "claude: add".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: Some("add".into()),
          harness: Some("claude".into()),
          model: None,
          started_at: Some(1),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(1.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("add".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 1,
          label: "codex: multiply".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: Some("multiply".into()),
          harness: Some("codex".into()),
          model: None,
          started_at: Some(1),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(1.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("multiply".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
        ProcRecord {
          index: 2,
          label: "grok: summarize".into(),
          kind: ProcKind::Skill,
          status: ProcStatus::Ok,
          skill_name: Some("summarize".into()),
          harness: Some("grok".into()),
          model: None,
          started_at: Some(2),
          note: None,
          detail: None,
          fail_reason: None,
          elapsed: Some(1.0),
          lines: vec![],
          container_name: None,
          cast_path: None,
          diff_path: None,
          skill_source: Some("summarize".into()),
          route: None,
          result_path: None,
          annotate_target: None,
        },
      ],
      last_seen_at: 1,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
    };
    let meta = effective_workflow_meta(&session).unwrap();
    let summarize = meta.nodes.iter().find(|n| n.id == "summarize").unwrap();
    assert_eq!(summarize.needs, vec!["add".to_string(), "multiply".to_string()]);
  }

  #[test]
  fn needs_from_profile_resolves_builtin_arith_for_any_repo() {
    let session = Session {
      id: "x".into(),
      started_at: 1,
      ended_at: None,
      profile: Some("arith".into()),
      kind: Some("workflow".into()),
      repo: "/Users/dima/.scsh/projects/test2".into(),
      branch: "main".into(),
      skills: vec![],
      procs: vec![],
      last_seen_at: 1,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
    };
    let map = needs_from_harness_profile(&session).expect("arith builtin");
    assert_eq!(map.get("summarize"), Some(&vec!["add".to_string(), "multiply".to_string()]));
  }

  #[test]
  fn builtins_yield_valid_workflow_meta() {
    for name in ["arith", "fruits", "code-review", "greet", "demo-loop-repeat", "demo-loop-do-while"] {
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
    assert!(review.when_summary.is_none(), "gate literals are not stored on graph metadata");
    let json = workflow_json(&meta);
    assert!(!json.contains("when_summary"), "when_summary is not emitted: {json}");
    assert!(!json.contains(" = "), "no gate literal expressions in JSON: {json}");
    // Legacy payloads with when_summary still parse; the field is dropped.
    let legacy = parse_workflow_value(Some(&crate::json::parse(
      r#"{ "nodes": [{ "id": "review", "order": 0, "needs": [], "conditional": true, "when_summary": "Runs only if secret = true" }] }"#,
    ).unwrap())).unwrap();
    assert!(legacy.nodes[0].conditional);
    assert!(legacy.nodes[0].when_summary.is_none());
    let (_, src) = crate::harness_def::builtin_defs().into_iter().find(|(n, _)| *n == "add").unwrap();
    let add = crate::harness_def::validate("add", src, crate::harness_def::DefSource::Builtin).unwrap();
    assert!(workflow_meta_from_def(&add).is_none());
  }

  fn node(id: &str, needs: &[&str]) -> WorkflowNodeMeta {
    WorkflowNodeMeta {
      id: id.into(),
      proc_index: None,
      order: 0,
      needs: needs.iter().map(|s| (*s).to_string()).collect(),
      conditional: false,
      when_summary: None,
    }
  }

  #[test]
  fn validator_rejects_every_malformed_class() {
    assert!(validate_workflow_meta(&WorkflowMeta { nodes: vec![] }).is_err(), "empty");
    assert!(validate_workflow_meta(&WorkflowMeta { nodes: vec![node("", &[])] }).is_err(), "empty id");
    assert!(validate_workflow_meta(&WorkflowMeta { nodes: vec![node("bad id!", &[])] }).is_err(), "unsafe id");
    assert!(
      validate_workflow_meta(&WorkflowMeta { nodes: vec![node("a", &[]), node("a", &[])] }).is_err(),
      "duplicate id"
    );
    assert!(validate_workflow_meta(&WorkflowMeta { nodes: vec![node("a", &["missing"])] }).is_err(), "unknown need");
    assert!(validate_workflow_meta(&WorkflowMeta { nodes: vec![node("a", &["a"])] }).is_err(), "self-edge");
    let dup_need = WorkflowMeta { nodes: vec![node("a", &[]), node("b", &["a", "a"])] };
    assert!(validate_workflow_meta(&dup_need).is_err(), "duplicate need");
    let two = WorkflowMeta {
      nodes: vec![
        WorkflowNodeMeta {
          id: "a".into(),
          proc_index: None,
          order: 0,
          needs: vec!["b".into()],
          conditional: false,
          when_summary: None,
        },
        WorkflowNodeMeta {
          id: "b".into(),
          proc_index: None,
          order: 1,
          needs: vec!["a".into()],
          conditional: false,
          when_summary: None,
        },
      ],
    };
    assert!(validate_workflow_meta(&two).is_err(), "two-node cycle");
    let long = WorkflowMeta { nodes: vec![node("a", &["c"]), node("b", &["a"]), node("c", &["b"])] };
    assert!(validate_workflow_meta(&long).is_err(), "longer cycle");
    let mut dup_proc = arith_meta();
    dup_proc.nodes[1].proc_index = Some(0);
    assert!(validate_workflow_meta(&dup_proc).is_err(), "duplicate proc_index");
  }

  #[test]
  fn bind_ignores_builds_and_unknown_steps() {
    let mut meta = arith_meta();
    bind_workflow_proc(&mut meta, "add", 99, ProcKind::Build);
    assert_eq!(meta.nodes[0].proc_index, Some(0), "build bind ignored");
    bind_workflow_proc(&mut meta, "nope", 7, ProcKind::Skill);
    assert!(meta.nodes.iter().all(|n| n.proc_index != Some(7)), "unknown step ignored");
    bind_workflow_proc(&mut meta, "add", 42, ProcKind::Skill);
    assert_eq!(meta.nodes[0].proc_index, Some(42));
  }

  #[test]
  fn repeat_nodes_are_appended_only_as_iterations_start() {
    let (_, src) = crate::harness_def::builtin_defs().into_iter().find(|(n, _)| *n == "demo-loop-repeat").unwrap();
    let def = crate::harness_def::validate("demo-loop-repeat", src, crate::harness_def::DefSource::Builtin).unwrap();
    let mut meta = workflow_meta_from_def(&def).unwrap();
    assert_eq!(meta.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["initialize", "increment__repeat"]);
    bind_workflow_proc(&mut meta, "increment__repeat_1", 10, ProcKind::Skill);
    bind_workflow_proc(&mut meta, "increment__repeat_2", 11, ProcKind::Skill);
    let first = meta.nodes.iter().find(|n| n.id == "increment__repeat_1").unwrap();
    let second = meta.nodes.iter().find(|n| n.id == "increment__repeat_2").unwrap();
    assert_eq!(first.needs, ["initialize"]);
    assert_eq!(second.needs, ["increment__repeat_1"]);
    assert_eq!(first.proc_index, Some(10));
    assert_eq!(second.proc_index, Some(11));
    assert!(validate_workflow_meta(&meta).is_ok());
  }

  #[test]
  fn do_while_nodes_are_appended_only_as_iterations_start() {
    let (_, src) = crate::harness_def::builtin_defs().into_iter().find(|(n, _)| *n == "demo-loop-do-while").unwrap();
    let def = crate::harness_def::validate("demo-loop-do-while", src, crate::harness_def::DefSource::Builtin).unwrap();
    let mut meta = workflow_meta_from_def(&def).unwrap();
    assert_eq!(meta.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["initialize", "increment__while"]);
    bind_workflow_proc(&mut meta, "increment__while_1", 10, ProcKind::Skill);
    bind_workflow_proc(&mut meta, "increment__while_2", 11, ProcKind::Skill);
    let first = meta.nodes.iter().find(|n| n.id == "increment__while_1").unwrap();
    let second = meta.nodes.iter().find(|n| n.id == "increment__while_2").unwrap();
    assert_eq!(first.needs, ["initialize"]);
    assert_eq!(second.needs, ["increment__while_1"]);
    assert_eq!(first.proc_index, Some(10));
    assert_eq!(second.proc_index, Some(11));
    assert!(validate_workflow_meta(&meta).is_ok());
  }

  #[test]
  fn loop_iteration_ids_parse_for_both_loop_kinds() {
    assert_eq!(parse_loop_iteration_id("increment__repeat_3"), Some(("increment", "__repeat", 3)));
    assert_eq!(parse_loop_iteration_id("increment__while_1"), Some(("increment", "__while", 1)));
    assert_eq!(parse_loop_iteration_id("increment"), None);
    assert_eq!(parse_loop_iteration_id("increment__while_x"), None);
  }

  #[test]
  fn parser_rejects_malformed_json_shapes() {
    assert!(parse_workflow_value(None).is_none());
    assert!(parse_workflow_value(Some(&crate::json::parse(r#"{}"#).unwrap())).is_none(), "missing nodes");
    assert!(parse_workflow_value(Some(&crate::json::parse(r#"{ "nodes": {} }"#).unwrap())).is_none());
    assert!(parse_workflow_value(Some(&crate::json::parse(r#"{ "nodes": [1] }"#).unwrap())).is_none());
    assert!(
      parse_workflow_value(Some(&crate::json::parse(r#"{ "nodes": [{ "order": 0 }] }"#).unwrap())).is_none(),
      "missing id"
    );
    assert!(
      parse_workflow_value(Some(
        &crate::json::parse(r#"{ "nodes": [{ "id": "a", "order": -1, "needs": [] }] }"#).unwrap()
      ))
      .is_none(),
      "negative order"
    );
    assert!(
      parse_workflow_value(Some(
        &crate::json::parse(r#"{ "nodes": [{ "id": "a", "order": 1.5, "needs": [] }] }"#).unwrap()
      ))
      .is_none(),
      "fractional order"
    );
    assert!(
      parse_workflow_value(Some(
        &crate::json::parse(r#"{ "nodes": [{ "id": "a", "order": 0, "proc_index": "x", "needs": [] }] }"#).unwrap()
      ))
      .is_none(),
      "wrong proc_index type"
    );
    assert!(
      parse_workflow_value(Some(
        &crate::json::parse(r#"{ "nodes": [{ "id": "a", "order": 0, "needs": [1] }] }"#).unwrap()
      ))
      .is_none(),
      "mixed needs"
    );
    assert!(
      parse_workflow_value(Some(
        &crate::json::parse(r#"{ "nodes": [{ "id": "a", "order": 0, "conditional": "yes", "needs": [] }] }"#).unwrap()
      ))
      .is_none(),
      "wrong conditional type"
    );
    // Unknown future fields are ignored.
    let ok = parse_workflow_value(Some(
      &crate::json::parse(r#"{ "nodes": [{ "id": "a", "order": 0, "needs": [], "future_field": true }] }"#).unwrap(),
    ));
    assert!(ok.is_some());
  }
}
