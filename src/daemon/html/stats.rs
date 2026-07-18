//! Stats tab: the flaky-route dashboard. Renders `~/.scsh/stats.jsonl` (already collected
//! by every run — see `crate::stats`) as two reliability tables: every harness · model
//! route across all skills ("which route flakes"), then each skill × route ("which
//! reviewer on which route"). Server-rendered from the durable file at page load; there
//! is nothing live to tick — the file only grows when a run finishes.

use super::escape::esc;
use super::format::{format_elapsed_clock, format_short_age};
use crate::daemon::paths::now_unix_secs;
use crate::stats::{flakiness_rows, FlakinessRow};

pub(crate) fn stats_panel() -> String {
  let records = crate::stats::read_records();
  let by_route = flakiness_rows(&records, |r| r.route_label());
  let by_skill = flakiness_rows(&records, |r| {
    let skill = r.skill_source.clone().or_else(|| r.skill.clone()).unwrap_or_else(|| "?".into());
    format!("{skill} — {}", r.route_label())
  });
  if by_route.is_empty() {
    return "<div class=\"chamfer card card--accent-left-cyan\">\n\
<p class=\"section-label\">Route reliability</p>\n\
<p class=\"dim\">No run statistics yet — every finished <code>scsh run</code> appends to \
<code>~/.scsh/stats.jsonl</code>, and this dashboard fills in from there.</p>\n</div>\n"
      .into();
  }
  format!(
    "{}\n{}",
    stats_card("Route reliability", "harness · model, across every skill", &by_route),
    stats_card("Skill × route", "each skill on each route it runs", &by_skill),
  )
}

fn stats_card(title: &str, subtitle: &str, rows: &[FlakinessRow]) -> String {
  let now = now_unix_secs();
  let mut body = String::new();
  for r in rows {
    body.push_str(&stats_row_html(r, now));
  }
  format!(
    "<div class=\"chamfer card card--accent-left-cyan\">\n\
<p class=\"section-label\">{title} <span class=\"dim\">· {subtitle} · durations exclude cache hits</span></p>\n\
<div class=\"table-scroll\"><table class=\"stats-table\">\n\
<thead><tr><th>Route</th><th>Runs</th><th>OK</th><th>Fail</th><th>Cache</th><th>Retry</th>\
<th>Fail %</th><th>p50</th><th>p95</th><th>Top failure</th><th>Last run</th></tr></thead>\n\
<tbody>\n{body}</tbody>\n</table></div>\n</div>\n",
    title = esc(title),
    subtitle = esc(subtitle),
    body = body,
  )
}

fn stats_row_html(r: &FlakinessRow, now: u64) -> String {
  let executed = r.ok + r.failed;
  let fail_pct = if executed == 0 { "—".to_string() } else { format!("{:.0}%", r.fail_pct()) };
  // Color speaks first: a route above 20% failures is a problem, above zero is worth a
  // glance, at zero it stays quiet.
  let fail_class = if r.fail_pct() >= 20.0 {
    " class=\"stats-fail-high\""
  } else if r.failed > 0 {
    " class=\"stats-fail-some\""
  } else {
    ""
  };
  let (p50, p95) = if executed == 0 {
    ("—".to_string(), "—".to_string())
  } else {
    (format_elapsed_clock(r.p50_secs), format_elapsed_clock(r.p95_secs))
  };
  let top = match &r.top_fail_reason {
    Some((reason, n)) => format!("<code>{}</code> ×{n}", esc(reason)),
    None => "<span class=\"dim\">—</span>".into(),
  };
  let last = if r.last_ts == 0 || now < r.last_ts {
    "—".to_string()
  } else {
    format!("{} ago", format_short_age(now - r.last_ts))
  };
  format!(
    "<tr><td class=\"stats-key\"><code>{key}</code></td><td>{runs}</td><td>{ok}</td><td>{fail}</td>\
<td>{cache}</td><td>{retry}</td><td{fail_class}>{fail_pct}</td><td>{p50}</td><td>{p95}</td>\
<td>{top}</td><td class=\"dim\">{last}</td></tr>\n",
    key = esc(&r.key),
    runs = r.runs,
    ok = r.ok,
    fail = r.failed,
    cache = r.cached,
    retry = r.retried,
    fail_class = fail_class,
    fail_pct = fail_pct,
    p50 = esc(&p50),
    p95 = esc(&p95),
    top = top,
    last = esc(&last),
  )
}

/// `GET /api/v1/stats` — the same two aggregations as JSON, for scripts.
pub fn stats_json() -> String {
  let records = crate::stats::read_records();
  let by_route = flakiness_rows(&records, |r| r.route_label());
  let by_skill = flakiness_rows(&records, |r| {
    let skill = r.skill_source.clone().or_else(|| r.skill.clone()).unwrap_or_else(|| "?".into());
    format!("{skill} — {}", r.route_label())
  });
  format!("{{ \"routes\": [{}], \"skills\": [{}] }}", rows_json(&by_route), rows_json(&by_skill))
}

fn rows_json(rows: &[FlakinessRow]) -> String {
  rows
    .iter()
    .map(|r| {
      let top = match &r.top_fail_reason {
        Some((reason, n)) => {
          format!("{{ \"reason\": {}, \"count\": {n} }}", crate::json::quote(reason))
        }
        None => "null".into(),
      };
      format!(
        "{{ \"key\": {}, \"runs\": {}, \"ok\": {}, \"failed\": {}, \"cached\": {}, \"retried\": {}, \
\"fail_pct\": {:.2}, \"p50_secs\": {:.3}, \"p95_secs\": {:.3}, \"last_ts\": {}, \"top_fail_reason\": {top} }}",
        crate::json::quote(&r.key),
        r.runs,
        r.ok,
        r.failed,
        r.cached,
        r.retried,
        r.fail_pct(),
        r.p50_secs,
        r.p95_secs,
        r.last_ts,
      )
    })
    .collect::<Vec<_>>()
    .join(", ")
}
