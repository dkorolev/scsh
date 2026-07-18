//! Fleet comparison sections on the live job page — side-by-side routes that share a
//! `skill_source` (matrix fleets). Grouping/parsing lives in `crate::fleet`; this module
//! only renders HTML.

use super::escape::esc;
use super::format::format_elapsed_clock;
use super::proc::status_glyph;
use crate::daemon::model::Session;
use crate::fleet::{fleet_groups, fleet_verdict, FleetGroup, FleetRoute, FleetVerdict};

/// Render each comparison table after the last proc it summarizes. A completed comparison
/// should read as a recap of the work immediately above it, not as a block of future knowledge
/// collected at the top of the page.
pub(crate) fn fleet_sections_by_anchor(session: &Session) -> std::collections::BTreeMap<usize, String> {
  let groups = fleet_groups(&session.procs);
  let mut anchored = std::collections::BTreeMap::new();
  for group in &groups {
    let Some(anchor) = group.routes.iter().map(|route| route.proc_index).max() else { continue };
    let out = anchored.entry(anchor).or_insert_with(|| String::from("<div class=\"fleets\">\n"));
    out.push_str(&fleet_group_html(group));
  }
  // Job-level verdict: one closing recap under the final comparison, counting every
  // route across every fleet. Only when the job ran more than one fleet — a single
  // group's own summary line already is that recap.
  if groups.len() >= 2 {
    if let (Some(verdict), Some(last)) = (fleet_verdict(&groups), anchored.keys().max().copied()) {
      if let Some(out) = anchored.get_mut(&last) {
        out.push_str(&fleet_verdict_html(&verdict, groups.len()));
      }
    }
  }
  for out in anchored.values_mut() {
    out.push_str("</div>\n");
  }
  anchored
}

/// The whole-run recap: route counts plus the grade histogram and mean a reader needs
/// to judge a review fleet at a glance. The counts span ticks live (client JS); the
/// grade half needs the daemon's result files, so it refreshes on server renders.
fn fleet_verdict_html(v: &FleetVerdict, fleets: usize) -> String {
  let mut counts = format!("{} ok, {} fail", v.ok, v.fail);
  if v.pending > 0 {
    counts.push_str(&format!(", {} pending", v.pending));
  }
  let mut grades = String::new();
  for (grade, n) in &v.grades {
    grades.push_str(&format!(" · {} ×{n}", esc(grade)));
  }
  if let Some(mean) = v.mean_score {
    grades.push_str(&format!(" · mean {mean:.2}"));
  }
  if v.findings_total > 0 {
    grades.push_str(&format!(" · {} finding{}", v.findings_total, if v.findings_total == 1 { "" } else { "s" }));
  }
  format!(
    r#"<section class="chamfer fleet fleet-verdict" data-fleet-verdict>
<h3 class="fleet-title">Fleet verdict <span class="dim">· {fleets} skills · {routes} routes</span></h3>
<p class="fleet-summary"><span class="fv-counts">{counts}</span><span class="fv-grades">{grades}</span></p>
</section>
"#,
    fleets = fleets,
    routes = v.routes,
    counts = esc(&counts),
    grades = grades,
  )
}

fn fleet_group_html(g: &FleetGroup) -> String {
  let cycle_iterations = g.routes.iter().all(|route| {
    crate::daemon::workflow::parse_loop_iteration_id(&route.route).is_some_and(|(base, _, _)| base == g.skill_source)
  });
  let (count_label, column_label) =
    if cycle_iterations { ("cycle iterations", "Cycle iteration") } else { ("routes", "Route") };
  let summary = if cycle_iterations {
    g.summary
      .replace("all routes agree", "all cycle iterations agree")
      .replace("across routes", "across cycle iterations")
      .replace(&format!("{} routes", g.routes.len()), &format!("{} cycle iterations", g.routes.len()))
  } else {
    g.summary.clone()
  };
  let mut rows = String::new();
  for r in &g.routes {
    rows.push_str(&fleet_row_html(r));
  }
  format!(
    r#"<section class="chamfer fleet" data-skill-source="{src}">
<h3 class="fleet-title"><code>{src}</code> <span class="dim">· {n} {count_label}</span></h3>
<p class="fleet-summary dim">{summary}</p>
<table class="fleet-compare">
<thead><tr><th>{column_label}</th><th>Status</th><th>Duration</th><th>Result</th><th></th></tr></thead>
<tbody>
{rows}</tbody>
</table>
</section>
"#,
    src = esc(&g.skill_source),
    n = g.routes.len(),
    count_label = count_label,
    column_label = column_label,
    summary = esc(&summary),
    rows = rows,
  )
}

fn fleet_row_html(r: &FleetRoute) -> String {
  let status_class = r.status.as_str();
  let status_label = r.status.as_str();
  let elapsed = r.elapsed.map(format_elapsed_clock).unwrap_or_else(|| "—".into());
  let result = fleet_result_cell(r);
  let model = r.model.as_deref().map(|m| format!(" <span class=\"dim\">({})</span>", esc(m))).unwrap_or_default();
  format!(
    r#"<tr class="fleet-row {status_class}">
<td class="fleet-route"><code>{route}</code>{model}<div class="dim fleet-harness">{harness}</div></td>
<td class="fleet-status"><span class="glyph">{glyph}</span> {status}</td>
<td class="fleet-elapsed">{elapsed}</td>
<td class="fleet-result">{result}</td>
<td class="fleet-jump-cell"><button type="button" class="chamfer fleet-jump" data-proc="{idx}" title="Open this step">↗</button></td>
</tr>
"#,
    status_class = status_class,
    route = esc(&r.route),
    model = model,
    harness = esc(&r.harness),
    glyph = status_glyph(r.status),
    status = status_label,
    elapsed = esc(&elapsed),
    result = result,
    idx = r.proc_index,
  )
}

fn fleet_result_cell(r: &FleetRoute) -> String {
  if let Some(g) = &r.grade {
    // Workflow-def reviews count `comments`; the code-review skills count `issues` —
    // show whichever this route reported.
    let (n, noun) = match (r.comments_count, r.issues_found) {
      (Some(n), _) => (n, "comment"),
      (None, Some(n)) => (n, "issue"),
      (None, None) => (0, "comment"),
    };
    return format!(
      "<span class=\"fleet-grade\">Grade: {}, {} {}{}.</span>",
      esc(g),
      n,
      noun,
      if n == 1 { "" } else { "s" }
    );
  }
  let mut parts = Vec::new();
  if let Some(n) = r.issues_found {
    parts.push(format!("<span class=\"fleet-issues\">{n} issue{}</span>", if n == 1 { "" } else { "s" }));
  }
  if let Some(m) = r.result_message.as_deref().or(r.detail.as_deref()) {
    if !m.is_empty() {
      parts.push(format!("<span class=\"fleet-msg\">{}</span>", esc(m)));
    }
  }
  if parts.is_empty() {
    return "<span class=\"dim\">—</span>".into();
  }
  parts.join(" · ")
}
