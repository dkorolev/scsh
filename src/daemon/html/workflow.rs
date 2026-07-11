//! Server-rendered workflow dependency graph for session pages.

use super::escape::esc;
use super::format::format_elapsed_clock;
use super::proc::proc_elapsed_secs;
use crate::daemon::model::{ProcRecord, Session};
use crate::daemon::workflow::{
  display_state, node_ranks, unmet_needs, validate_workflow_meta, WorkflowDisplayState, WorkflowMeta, WorkflowNodeMeta,
};

const NODE_W: f64 = 200.0;
const NODE_H: f64 = 72.0;
const GAP_X: f64 = 56.0;
const GAP_Y: f64 = 28.0;
const PAD: f64 = 16.0;

#[derive(Clone)]
struct LaidOut {
  id: String,
  x: f64,
  y: f64,
  order: usize,
}

/// Workflow card HTML, or empty when the session has no valid graph metadata.
pub(crate) fn workflow_graph_html(session: &Session, now: u64) -> String {
  let Some(meta) = session.workflow.as_ref() else {
    return String::new();
  };
  if validate_workflow_meta(meta).is_err() {
    return String::new();
  }
  // Prefer explicit workflow kind; also show when metadata alone is present (browser pre-create).
  if session.kind.as_deref() != Some("workflow") && session.kind.is_some() {
    return String::new();
  }

  let layout = layout_nodes(meta);
  let width = layout.iter().map(|n| n.x + NODE_W).fold(0.0_f64, f64::max) + PAD;
  let height = layout.iter().map(|n| n.y + NODE_H).fold(0.0_f64, f64::max) + PAD;
  let by_id: std::collections::BTreeMap<&str, &LaidOut> = layout.iter().map(|n| (n.id.as_str(), n)).collect();

  let edges_svg = render_edges(meta, &by_id);

  let mut nodes_html = String::new();
  let mut present = std::collections::BTreeSet::new();
  let mut counts = StatusCounts::default();
  for node in &meta.nodes {
    let Some(pos) = by_id.get(node.id.as_str()) else {
      continue;
    };
    let state = display_state(session, node, now);
    present.insert(state);
    counts.tally(state);
    nodes_html.push_str(&node_html(session, node, pos, now));
  }

  format!(
    r#"<div class="card card--accent-left-cyan workflow-card" id="workflow-graph" data-workflow-graph>
<div class="workflow-head">
<h2 class="workflow-title">Workflow</h2>
<p class="workflow-summary dim">{summary}</p>
{legend}
</div>
<div class="workflow-scroll">
<div class="workflow-stage" style="width:{w:.0}px;height:{h:.0}px">
<svg class="workflow-edges" width="{w:.0}" height="{h:.0}" viewBox="0 0 {w:.1} {h:.1}" aria-hidden="true">
<defs>
<marker id="wf-arrow" viewBox="0 0 14 14" refX="12" refY="7" markerWidth="9" markerHeight="9" orient="auto" markerUnits="userSpaceOnUse">
<path class="wf-arrowhead" d="M3.5 2.5 L11 7 L3.5 11.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>
</marker>
</defs>
{edges}
</svg>
<div class="workflow-nodes">{nodes}</div>
</div>
</div>
</div>
"#,
    summary = counts.summary_text(meta.nodes.len()),
    legend = legend_html(&present),
    w = width,
    h = height,
    edges = edges_svg,
    nodes = nodes_html,
  )
}

/// One dependency edge after port assignment (distinct exit/entry y when a node fans in/out).
struct EdgeGeom {
  x1: f64,
  y1: f64,
  x2: f64,
  y2: f64,
}

/// Draw dependency edges: horizontal tangents at both ends, fan-in/out ports spaced along the
/// node sides (so two arrows into `summarize` do not share one tip), open chevron heads.
fn render_edges(meta: &WorkflowMeta, by_id: &std::collections::BTreeMap<&str, &LaidOut>) -> String {
  // Collect (src_id, dst_id) in stable order.
  let mut pairs: Vec<(&str, &str)> = Vec::new();
  for node in &meta.nodes {
    if !by_id.contains_key(node.id.as_str()) {
      continue;
    }
    for need in &node.needs {
      if by_id.contains_key(need.as_str()) {
        pairs.push((need.as_str(), node.id.as_str()));
      }
    }
  }

  // Outgoing / incoming multiplicity per node (for port spacing).
  let mut out_n: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
  let mut in_n: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
  for &(s, d) in &pairs {
    *out_n.entry(s).or_default() += 1;
    *in_n.entry(d).or_default() += 1;
  }

  // Sort each node's edges by the other end's y so ports top→bottom match visual order.
  let mut out_rank: std::collections::BTreeMap<&str, Vec<usize>> = std::collections::BTreeMap::new();
  let mut in_rank: std::collections::BTreeMap<&str, Vec<usize>> = std::collections::BTreeMap::new();
  for (i, &(s, d)) in pairs.iter().enumerate() {
    out_rank.entry(s).or_default().push(i);
    in_rank.entry(d).or_default().push(i);
  }
  for idxs in out_rank.values_mut() {
    idxs.sort_by(|&a, &b| {
      let ya = by_id.get(pairs[a].1).map(|n| n.y).unwrap_or(0.0);
      let yb = by_id.get(pairs[b].1).map(|n| n.y).unwrap_or(0.0);
      ya.partial_cmp(&yb).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.cmp(&b))
    });
  }
  for idxs in in_rank.values_mut() {
    idxs.sort_by(|&a, &b| {
      let ya = by_id.get(pairs[a].0).map(|n| n.y).unwrap_or(0.0);
      let yb = by_id.get(pairs[b].0).map(|n| n.y).unwrap_or(0.0);
      ya.partial_cmp(&yb).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.cmp(&b))
    });
  }

  let mut out_port: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
  let mut in_port: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
  for idxs in out_rank.values() {
    for (port, &ei) in idxs.iter().enumerate() {
      out_port.insert(ei, port);
    }
  }
  for idxs in in_rank.values() {
    for (port, &ei) in idxs.iter().enumerate() {
      in_port.insert(ei, port);
    }
  }

  let mut svg = String::new();
  for (i, &(s, d)) in pairs.iter().enumerate() {
    let src = by_id[s];
    let dst = by_id[d];
    let geom = EdgeGeom {
      x1: src.x + NODE_W,
      y1: port_y(src.y, out_port[&i], out_n[s]),
      x2: dst.x,
      y2: port_y(dst.y, in_port[&i], in_n[d]),
    };
    svg.push_str(&edge_path(&geom));
  }
  svg
}

/// Vertical attachment along a node face: single edge → center; several → evenly spaced
/// with a margin so tips do not sit on the corners.
fn port_y(node_y: f64, index: usize, count: usize) -> f64 {
  if count <= 1 {
    return node_y + NODE_H / 2.0;
  }
  let margin = NODE_H * 0.22;
  let usable = NODE_H - 2.0 * margin;
  node_y + margin + usable * (index as f64) / ((count - 1) as f64)
}

/// Cubic with horizontal tangents at both ends — reads as a clean ribbon, not a merged Y.
fn edge_path(e: &EdgeGeom) -> String {
  let dx = (e.x2 - e.x1).max(24.0);
  let c1x = e.x1 + dx * 0.42;
  let c2x = e.x2 - dx * 0.42;
  // Stop a hair short of the node so the open chevron sits in the gutter, not under the border.
  let x2 = e.x2 - 1.5;
  format!(
    r#"<path class="wf-edge" d="M{x1:.1},{y1:.1} C{c1x:.1},{y1:.1} {c2x:.1},{y2:.1} {x2:.1},{y2:.1}" marker-end="url(#wf-arrow)" />"#,
    x1 = e.x1,
    y1 = e.y1,
    y2 = e.y2,
  )
}

#[derive(Default)]
struct StatusCounts {
  done: usize,
  running: usize,
  waiting: usize,
  failed: usize,
  stalled: usize,
  skipped: usize,
}

impl StatusCounts {
  fn tally(&mut self, state: WorkflowDisplayState) {
    match state {
      WorkflowDisplayState::Done => self.done += 1,
      WorkflowDisplayState::Running => self.running += 1,
      // Ready is "waiting, prerequisites met" — count with waiting in the headline.
      WorkflowDisplayState::Waiting | WorkflowDisplayState::Ready => self.waiting += 1,
      WorkflowDisplayState::Failed => self.failed += 1,
      WorkflowDisplayState::Stalled => self.stalled += 1,
      WorkflowDisplayState::Skipped => self.skipped += 1,
    }
  }

  /// e.g. `3 tasks · 2 done · 1 running` — only non-zero status buckets.
  fn summary_text(&self, total: usize) -> String {
    let mut parts = vec![format!("{total} {}", if total == 1 { "task" } else { "tasks" })];
    for (n, label) in [
      (self.done, "done"),
      (self.running, "running"),
      (self.waiting, "waiting"),
      (self.failed, "failed"),
      (self.stalled, "stalled"),
      (self.skipped, "skipped"),
    ] {
      if n > 0 {
        parts.push(format!("{n} {label}"));
      }
    }
    parts.join(" · ")
  }
}

/// Legend entries only for states that appear on at least one node (stable display order).
fn legend_html(present: &std::collections::BTreeSet<WorkflowDisplayState>) -> String {
  // Fixed order so the legend does not jump as states come and go.
  const ORDER: &[WorkflowDisplayState] = &[
    WorkflowDisplayState::Running,
    WorkflowDisplayState::Done,
    WorkflowDisplayState::Failed,
    WorkflowDisplayState::Stalled,
    WorkflowDisplayState::Waiting,
    WorkflowDisplayState::Ready,
    WorkflowDisplayState::Skipped,
  ];
  let mut items = String::new();
  for state in ORDER {
    if !present.contains(state) {
      continue;
    }
    items.push_str(&format!(
      r#"<li class="wf-leg wf-leg-{key}"><span class="wf-ico" aria-hidden="true">{ico}</span> {label}</li>"#,
      key = state.as_str(),
      ico = state_icon(*state),
      label = state.label(),
    ));
  }
  if items.is_empty() {
    return String::new();
  }
  format!(r#"<ul class="workflow-legend" aria-label="Status legend">{items}</ul>"#)
}

fn layout_nodes(meta: &WorkflowMeta) -> Vec<LaidOut> {
  let ranks = node_ranks(meta);
  let mut by_rank: std::collections::BTreeMap<usize, Vec<usize>> = std::collections::BTreeMap::new();
  for (i, &r) in ranks.iter().enumerate() {
    by_rank.entry(r).or_default().push(i);
  }
  for idxs in by_rank.values_mut() {
    idxs.sort_by_key(|&i| meta.nodes[i].order);
  }
  let max_in_rank = by_rank.values().map(|v| v.len()).max().unwrap_or(1);
  let col_h = max_in_rank as f64 * NODE_H + (max_in_rank.saturating_sub(1) as f64) * GAP_Y;
  let mut out = Vec::with_capacity(meta.nodes.len());
  for (rank, idxs) in &by_rank {
    let n = idxs.len();
    let block_h = n as f64 * NODE_H + (n.saturating_sub(1) as f64) * GAP_Y;
    let y0 = PAD + (col_h - block_h) / 2.0;
    let x = PAD + (*rank as f64) * (NODE_W + GAP_X);
    for (row, &i) in idxs.iter().enumerate() {
      let node = &meta.nodes[i];
      out.push(LaidOut { id: node.id.clone(), x, y: y0 + row as f64 * (NODE_H + GAP_Y), order: node.order });
    }
  }
  out.sort_by(|a, b| a.order.cmp(&b.order));
  out
}

fn node_html(session: &Session, node: &WorkflowNodeMeta, pos: &LaidOut, now: u64) -> String {
  let state = display_state(session, node, now);
  let proc = node.proc_index.and_then(|i| session.procs.iter().find(|p| p.index == i));
  let harness = proc.and_then(|p| p.harness.as_deref()).unwrap_or("");
  let model = proc.and_then(|p| p.model.as_deref()).unwrap_or("");
  let elapsed = proc.and_then(|p| proc_elapsed_secs(p, now)).map(format_elapsed_clock);
  let unmet = unmet_needs(session, node);
  let deps = if node.needs.is_empty() {
    "no dependencies".to_string()
  } else {
    format!("depends on {}", node.needs.join(" and "))
  };
  let aria = format!("{}, {}, {deps}", node.id, state.label());
  let mut meta_bits = Vec::new();
  if !harness.is_empty() {
    meta_bits.push(esc(harness));
  }
  if !model.is_empty() {
    meta_bits.push(esc(model));
  }
  if let Some(ref e) = elapsed {
    if matches!(
      state,
      WorkflowDisplayState::Running
        | WorkflowDisplayState::Done
        | WorkflowDisplayState::Failed
        | WorkflowDisplayState::Stalled
    ) {
      meta_bits.push(esc(e));
    }
  }
  if state == WorkflowDisplayState::Waiting && unmet > 0 {
    meta_bits.push(format!("{unmet} waiting on"));
  }
  if state == WorkflowDisplayState::Ready {
    meta_bits.push("ready".into());
  }
  let gate = if node.conditional {
    let tip = node.when_summary.as_deref().unwrap_or("Runs only if its when: gate holds");
    format!(r#"<span class="wf-gate" title="{t}" aria-label="{t}">when</span>"#, t = esc(tip))
  } else {
    String::new()
  };
  let href = format!("#task-{}", esc(&node.id));
  let proc_attr = match node.proc_index {
    Some(i) => format!(r#" data-proc-index="{i}""#),
    None => String::new(),
  };
  format!(
    r#"<a class="wf-node wf-{state}" href="{href}" id="wf-node-{id}" data-workflow-step="{id}" data-wf-state="{state}"{proc_attr} style="left:{x:.1}px;top:{y:.1}px;width:{w:.0}px;min-height:{h:.0}px" aria-label="{aria}">
<span class="wf-state"><span class="wf-ico" aria-hidden="true">{ico}</span><span class="wf-state-label">{label}</span></span>
<span class="wf-id">{id_esc}{gate}</span>
<span class="wf-meta dim">{meta}</span>
</a>"#,
    state = state.as_str(),
    href = href,
    id = esc(&node.id),
    id_esc = esc(&node.id),
    proc_attr = proc_attr,
    x = pos.x,
    y = pos.y,
    w = NODE_W,
    h = NODE_H,
    aria = esc(&aria),
    ico = state_icon(state),
    label = state.label(),
    gate = gate,
    meta = meta_bits.join(" · "),
  )
}

fn state_icon(state: WorkflowDisplayState) -> &'static str {
  match state {
    WorkflowDisplayState::Waiting | WorkflowDisplayState::Ready => "○",
    WorkflowDisplayState::Running => "◉",
    WorkflowDisplayState::Done => "✓",
    WorkflowDisplayState::Failed => "✗",
    WorkflowDisplayState::Skipped => "⊘",
    WorkflowDisplayState::Stalled => "!",
  }
}

/// Stable task anchor attributes for a proc panel when it maps to a workflow step.
pub(crate) fn proc_task_attrs(session: &Session, proc: &ProcRecord) -> String {
  let Some(meta) = session.workflow.as_ref() else {
    return String::new();
  };
  let Some(node) = meta.nodes.iter().find(|n| n.proc_index == Some(proc.index)) else {
    let step = proc.skill_name.as_deref().or(proc.skill_source.as_deref());
    let Some(step) = step else {
      return String::new();
    };
    if !meta.nodes.iter().any(|n| n.id == step) {
      return String::new();
    }
    return format!(r#" id="task-{id}" data-workflow-step="{id}""#, id = esc(step));
  };
  format!(r#" id="task-{id}" data-workflow-step="{id}""#, id = esc(&node.id))
}
