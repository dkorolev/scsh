//! Fleet comparison sections on the live job page — side-by-side routes that share a
//! `skill_source` (matrix fleets). Grouping/parsing lives in `crate::fleet`; this module
//! only renders HTML.

use super::escape::esc;
use super::format::format_elapsed_clock;
use super::proc::status_glyph;
use crate::daemon::model::Session;
use crate::fleet::{fleet_groups, FleetGroup, FleetRoute};

/// Render comparison tables for every multi-route `skill_source` in the session.
/// Empty when the job has no fleets (every skill ran a single route).
pub(crate) fn fleet_sections_html(session: &Session) -> String {
  let groups = fleet_groups(&session.procs);
  if groups.is_empty() {
    return String::new();
  }
  let mut out = String::from("<div class=\"fleets\">\n");
  for g in &groups {
    out.push_str(&fleet_group_html(g));
  }
  out.push_str("</div>\n");
  out
}

fn fleet_group_html(g: &FleetGroup) -> String {
  let mut rows = String::new();
  for r in &g.routes {
    rows.push_str(&fleet_row_html(r));
  }
  format!(
    r#"<section class="fleet" data-skill-source="{src}">
<h3 class="fleet-title"><code>{src}</code> <span class="dim">· {n} routes</span></h3>
<p class="fleet-summary dim">{summary}</p>
<table class="fleet-compare">
<thead><tr><th>Route</th><th>Status</th><th>Duration</th><th>Result</th><th></th></tr></thead>
<tbody>
{rows}</tbody>
</table>
</section>
"#,
    src = esc(&g.skill_source),
    n = g.routes.len(),
    summary = esc(&g.summary),
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
<td class="fleet-jump-cell"><button type="button" class="fleet-jump" data-proc="{idx}" title="Open this step">↗</button></td>
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
  let mut parts = Vec::new();
  if let Some(g) = &r.grade {
    parts.push(format!("<span class=\"fleet-grade\">grade {}</span>", esc(g)));
  }
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
