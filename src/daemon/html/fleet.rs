//! Fleet comparison sections on the live job page — side-by-side routes that share a
//! `skill_source` (matrix fleets). Grouping/parsing lives in `crate::fleet`; this module
//! only renders HTML.

use super::escape::esc;
use super::format::format_elapsed_clock;
use super::proc::status_glyph;
use crate::daemon::model::Session;
use crate::fleet::{fleet_groups, fleet_verdict, job_rounds, FleetGroup, FleetRoute, FleetVerdict, RoundSummary};

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
  // A scoring loop's convergence: one row per cycle, under the last cycle that reported —
  // the same recap-in-place rule as the comparisons above. Without it a loop's scores live
  // only inside per-cycle result files, which is what made a stalled loop unreadable.
  let rounds = job_rounds(&session.procs);
  if let Some(anchor) = rounds.iter().map(|r| r.proc_index).max() {
    let out = anchored.entry(anchor).or_insert_with(|| String::from("<div class=\"fleets\">\n"));
    out.push_str(&rounds_html(&rounds));
  }
  for out in anchored.values_mut() {
    out.push_str("</div>\n");
  }
  anchored
}

/// The convergence table: each cycle's mean (with its move against the cycle before), grade
/// histogram, and the round's own verdict. Parsed from result files on the daemon, so — like
/// the fleet verdict's grade half — it refreshes on server renders rather than on ticks.
fn rounds_html(rounds: &[RoundSummary]) -> String {
  let mut rows = String::new();
  let mut previous: Option<f64> = None;
  for r in rounds {
    let mean = match r.mean {
      Some(m) => {
        let delta = match previous {
          // ±0.005 renders as "0.00", which reads as movement where there was none.
          Some(p) if (m - p).abs() >= 0.005 => {
            let arrow = if m > p { "▲" } else { "▼" };
            let class = if m > p { "round-up" } else { "round-down" };
            format!(" <span class=\"{class}\">{arrow} {:+.2}</span>", m - p)
          }
          Some(_) => " <span class=\"dim\">— flat</span>".to_string(),
          None => String::new(),
        };
        format!("<span class=\"round-mean\">{m:.2}</span>{delta}")
      }
      None => "<span class=\"dim\">—</span>".to_string(),
    };
    if r.mean.is_some() {
      previous = r.mean;
    }
    let grades = r.counts.iter().map(|(grade, n)| format!("{} ×{n}", esc(grade))).collect::<Vec<_>>().join(" · ");
    let verdict = match (r.verdict.as_deref(), r.approved) {
      (Some(v), Some(true)) => format!("<span class=\"round-met\">{}</span>", esc(v)),
      (Some(v), _) => format!("<span class=\"dim\">{}</span>", esc(v)),
      (None, Some(true)) => "<span class=\"round-met\">approved</span>".to_string(),
      (None, Some(false)) => "<span class=\"dim\">not approved</span>".to_string(),
      (None, None) => "<span class=\"dim\">—</span>".to_string(),
    };
    rows.push_str(&format!(
      r#"<tr class="round-row">
<td class="round-cycle">{cycle}</td>
<td class="round-mean-cell">{mean}</td>
<td class="round-grades">{grades}</td>
<td class="round-verdict">{verdict}</td>
<td class="fleet-jump-cell"><button type="button" class="chamfer fleet-jump" data-proc="{idx}" title="Open this cycle">↗</button></td>
</tr>
"#,
      cycle = r.iteration,
      mean = mean,
      grades = if grades.is_empty() { "<span class=\"dim\">—</span>".to_string() } else { grades },
      verdict = verdict,
      idx = r.proc_index,
    ));
  }
  let scored: Vec<f64> = rounds.iter().filter_map(|r| r.mean).collect();
  let trend = match (scored.first(), scored.last()) {
    (Some(first), Some(last)) if scored.len() >= 2 => {
      format!(" <span class=\"dim\">· {first:.2} → {last:.2}</span>")
    }
    _ => String::new(),
  };
  let step = rounds.first().map(|r| r.step.as_str()).unwrap_or_default();
  format!(
    r#"<section class="chamfer fleet fleet-rounds" data-fleet-rounds>
<h3 class="fleet-title">Loop convergence <span class="dim">· <code>{step}</code> · {n} cycle{plural}</span>{trend}</h3>
<table class="fleet-compare rounds-compare">
<thead><tr><th>Cycle</th><th>Mean</th><th>Grades</th><th>Verdict</th><th></th></tr></thead>
<tbody>
{rows}</tbody>
</table>
</section>
"#,
    step = esc(step),
    n = rounds.len(),
    plural = if rounds.len() == 1 { "" } else { "s" },
    trend = trend,
    rows = rows,
  )
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
