#![allow(dead_code)] // kept verbatim from upstream; only `to_html` is used here
//! Markdown → HTML for the text tasks contribute to a job page (its errors, results, and
//! log sections).
//!
//! This is packdiff's renderer, copied verbatim from `packdiff-dto` 0.9.1 (MIT, same
//! author; <https://github.com/dkorolev/packdiff>, `dto/src/markdown.rs`; pipe tables
//! landed there through dkorolev/packdiff#40 for this page) rather than taken as a
//! dependency: that crate carries serde, whose derive macros pull in
//! `unicode-ident` under a licence the plain-MIT audit in `licenses.rs` refuses. Keep
//! this file in step with upstream — fixes belong there first — and swap it for a
//! dependency the day packdiff ships the renderer as a dependency-free crate.
//!
//! Upstream's own description follows.
//!
//! Minimal, safety-first Markdown → HTML: used for comment bodies and for
//! the rendered view of markdown files on the generated page.
//!
//! Safety by construction: every input character is HTML-escaped first; the
//! only tags in the output are the ones this module emits, and link targets
//! are restricted to `http://` / `https://` / `mailto:` — hostile input
//! cannot smuggle markup or `javascript:` URLs.
//!
//! Deliberately a SUBSET (documented in docs/PAGE.md): ATX headings, fenced
//! code blocks, nested lists, blockquotes, thematic breaks, pipe tables,
//! paragraphs; inline `` `code` ``, `**bold**`, `*italic*`, and
//! `[links](https://…)`. Underscores are NOT emphasis, so `snake_case`
//! identifiers survive verbatim. A single newline inside a paragraph is a
//! hard break, matching how people write review comments.

/// Render markdown `text` to HTML. Pure and deterministic; never fails —
/// anything unrecognized falls back to escaped literal text.
pub fn to_html(text: &str) -> String {
  let lines: Vec<&str> = text.lines().collect();
  blocks_to_html(&lines, 0)
}

/// Like [`to_html`], but one entry per top-level block: `(line, html)`,
/// where `line` is the 0-based index (within `text`) of the block's first
/// line. Concatenating the `html` parts yields exactly `to_html(text)`.
/// This is what lets the page anchor a comment on a rendered markdown block
/// back to the source line the block starts at.
pub fn to_html_blocks(text: &str) -> Vec<(usize, String)> {
  let lines: Vec<&str> = text.lines().collect();
  let mut out = Vec::new();
  let mut i = 0;
  while i < lines.len() {
    if lines[i].trim_start().is_empty() {
      i += 1;
      continue;
    }
    let (next, html) = one_block(&lines, i, 0);
    out.push((i, html));
    i = next;
  }
  out
}

/// Like [`to_html_blocks`], but splits every list item into its own rendered
/// block. Review pages use this form so every bullet is an independently
/// commentable source line; nested items carry a safe numeric indentation
/// token that the page turns back into their visual nesting.
pub fn to_html_review_blocks(text: &str) -> Vec<(usize, String)> {
  let lines: Vec<&str> = text.lines().collect();
  let mut out = Vec::new();
  let mut i = 0;
  while i < lines.len() {
    if lines[i].trim_start().is_empty() {
      i += 1;
      continue;
    }
    if list_item(lines[i]).is_some() {
      let (next, items) = list_review_blocks(&lines, i, 0);
      out.extend(items);
      i = next;
    } else {
      let (next, html) = one_block(&lines, i, 0);
      out.push((i, html));
      i = next;
    }
  }
  out
}

/// Recursion cap for nested blockquotes and nested inline emphasis; markdown
/// past this depth renders as escaped literal text instead of overflowing.
const MAX_DEPTH: u32 = 8;

fn esc(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for ch in s.chars() {
    esc_char(ch, &mut out);
  }
  out
}

fn esc_char(ch: char, out: &mut String) {
  match ch {
    '&' => out.push_str("&amp;"),
    '<' => out.push_str("&lt;"),
    '>' => out.push_str("&gt;"),
    '"' => out.push_str("&quot;"),
    '\'' => out.push_str("&#x27;"),
    _ => out.push(ch),
  }
}

fn blocks_to_html(lines: &[&str], depth: u32) -> String {
  let mut out = String::new();
  let mut i = 0;
  while i < lines.len() {
    if lines[i].trim_start().is_empty() {
      i += 1;
      continue;
    }
    let (next, html) = one_block(lines, i, depth);
    out.push_str(&html);
    i = next;
  }
  out
}

/// Render the single block starting at the non-blank `lines[start]`; returns
/// the index just past the block and its HTML.
fn one_block(lines: &[&str], start: usize, depth: u32) -> (usize, String) {
  let mut i = start;
  let trimmed = lines[i].trim_start();
  if trimmed.starts_with("```") {
    // Fenced code block; an unclosed fence runs to the end of the input.
    let mut j = i + 1;
    let mut code = String::new();
    while j < lines.len() && !lines[j].trim_start().starts_with("```") {
      code.push_str(lines[j]);
      code.push('\n');
      j += 1;
    }
    return (j + 1, format!("<pre><code>{}</code></pre>", esc(&code)));
  }
  if let Some((level, content)) = heading(trimmed) {
    return (i + 1, format!("<h{level}>{}</h{level}>", inline(content, depth)));
  }
  if is_rule(trimmed) {
    return (i + 1, "<hr>".to_string());
  }
  if trimmed.starts_with('>') && depth < MAX_DEPTH {
    let mut inner: Vec<&str> = Vec::new();
    while i < lines.len() {
      let t = lines[i].trim_start();
      let Some(stripped) = t.strip_prefix('>') else { break };
      inner.push(stripped.strip_prefix(' ').unwrap_or(stripped));
      i += 1;
    }
    return (i, format!("<blockquote>{}</blockquote>", blocks_to_html(&inner, depth + 1)));
  }
  if list_item(lines[i]).is_some() {
    return list_block(lines, i, depth);
  }
  if table_starts_at(lines, i) {
    return table_block(lines, i, depth);
  }
  // Paragraph: consecutive plain lines; each single newline is a hard break.
  let mut parts: Vec<String> = Vec::new();
  while i < lines.len() {
    let t = lines[i].trim_start();
    if t.is_empty() || starts_block(t) || table_starts_at(lines, i) {
      break;
    }
    parts.push(inline(lines[i].trim_end(), depth));
    i += 1;
  }
  (i, format!("<p>{}</p>", parts.join("<br>")))
}

/// A pipe table starts where a row of cells is followed by its delimiter row
/// (`|---|:--:|`) with the same number of cells. Nothing else looks like one,
/// so a lone `a | b` line stays a paragraph.
fn table_starts_at(lines: &[&str], i: usize) -> bool {
  let Some(delimiter) = lines.get(i + 1) else { return false };
  if !lines[i].contains('|') {
    return false;
  }
  let header = table_cells(lines[i]);
  let aligns = table_alignments(delimiter);
  !header.is_empty() && aligns.len() == header.len()
}

/// Column alignment from a delimiter row: `:--` left, `--:` right, `:-:`
/// center, `---` unset. `None` when the row is not a delimiter row.
fn table_alignments(line: &str) -> Vec<Option<&'static str>> {
  if !line.contains('|') && !line.trim().contains('-') {
    return Vec::new();
  }
  let cells = table_cells(line);
  let mut out = Vec::with_capacity(cells.len());
  for cell in &cells {
    let c = cell.trim();
    let left = c.starts_with(':');
    let right = c.ends_with(':');
    let dashes = c.trim_start_matches(':').trim_end_matches(':');
    if dashes.is_empty() || !dashes.chars().all(|ch| ch == '-') {
      return Vec::new();
    }
    out.push(match (left, right) {
      (true, true) => Some("center"),
      (true, false) => Some("left"),
      (false, true) => Some("right"),
      (false, false) => None,
    });
  }
  out
}

/// The cells of one table row: split on pipes that are neither escaped
/// (`\|`) nor inside backticks, with the optional leading and trailing pipe
/// dropped and each cell trimmed.
fn table_cells(line: &str) -> Vec<String> {
  let trimmed = line.trim();
  let mut cells = Vec::new();
  let mut cell = String::new();
  let mut in_code = false;
  let mut chars = trimmed.chars().peekable();
  while let Some(ch) = chars.next() {
    match ch {
      '\\' if chars.peek() == Some(&'|') => {
        chars.next();
        cell.push('|');
      }
      '`' => {
        in_code = !in_code;
        cell.push(ch);
      }
      '|' if !in_code => cells.push(std::mem::take(&mut cell)),
      _ => cell.push(ch),
    }
  }
  cells.push(cell);
  if trimmed.starts_with('|') {
    cells.remove(0);
  }
  if trimmed.ends_with('|') && !trimmed.ends_with("\\|") && cells.len() > 1 {
    cells.pop();
  }
  cells.into_iter().map(|c| c.trim().to_string()).collect()
}

/// Render the table starting at `lines[start]` (its header row). Body rows
/// run until a blank line or a line without a pipe; short rows are padded,
/// long rows cut, to the header's width, as GitHub does.
fn table_block(lines: &[&str], start: usize, depth: u32) -> (usize, String) {
  let header = table_cells(lines[start]);
  let aligns = table_alignments(lines[start + 1]);
  let width = header.len();
  let cell_html = |tag: &str, text: &str, align: Option<&str>| {
    let style = align.map(|a| format!(" style=\"text-align:{a}\"")).unwrap_or_default();
    format!("<{tag}{style}>{}</{tag}>", inline(text, depth))
  };
  let mut html = String::from("<table><thead><tr>");
  for (cell, align) in header.iter().zip(&aligns) {
    html.push_str(&cell_html("th", cell, *align));
  }
  html.push_str("</tr></thead>");
  let mut i = start + 2;
  let mut body = String::new();
  while i < lines.len() {
    let line = lines[i];
    if line.trim().is_empty() || !line.contains('|') {
      break;
    }
    let mut cells = table_cells(line);
    cells.resize(width, String::new());
    body.push_str("<tr>");
    for (cell, align) in cells.iter().zip(&aligns) {
      body.push_str(&cell_html("td", cell, *align));
    }
    body.push_str("</tr>");
    i += 1;
  }
  if !body.is_empty() {
    html.push_str(&format!("<tbody>{body}</tbody>"));
  }
  html.push_str("</table>");
  (i, html)
}

/// True when the line begins some non-paragraph block, ending a paragraph.
fn starts_block(trimmed: &str) -> bool {
  trimmed.starts_with("```")
    || trimmed.starts_with('>')
    || heading(trimmed).is_some()
    || is_rule(trimmed)
    || unordered_item(trimmed).is_some()
    || ordered_item(trimmed).is_some()
}

/// `#{1,6} ` → `(level, content)`.
fn heading(trimmed: &str) -> Option<(usize, &str)> {
  let level = trimmed.bytes().take_while(|&b| b == b'#').count();
  if (1..=6).contains(&level) {
    if let Some(content) = trimmed[level..].strip_prefix(' ') {
      return Some((level, content.trim()));
    }
  }
  None
}

/// Three or more of the same `-` / `*` / `_` and nothing else.
fn is_rule(trimmed: &str) -> bool {
  let t: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
  t.len() >= 3 && (t.chars().all(|c| c == '-') || t.chars().all(|c| c == '*') || t.chars().all(|c| c == '_'))
}

/// `- item` / `* item` / `+ item` → the item text.
fn unordered_item(trimmed: &str) -> Option<&str> {
  for marker in ["- ", "* ", "+ "] {
    if let Some(rest) = trimmed.strip_prefix(marker) {
      return Some(rest);
    }
  }
  None
}

/// `12. item` → the item text.
fn ordered_item(trimmed: &str) -> Option<&str> {
  let digits = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
  if digits == 0 {
    return None;
  }
  trimmed[digits..].strip_prefix(". ")
}

#[derive(Clone, Copy, PartialEq)]
enum ListKind {
  Unordered,
  Ordered,
}

/// Leading byte indentation, marker kind, and item content. Markdown list
/// indentation is relative, so any deeper run nests beneath the current item.
fn list_item(line: &str) -> Option<(usize, ListKind, &str)> {
  let trimmed = line.trim_start_matches([' ', '\t']);
  let indent = line.len() - trimmed.len();
  if let Some(item) = unordered_item(trimmed) {
    Some((indent, ListKind::Unordered, item))
  } else {
    ordered_item(trimmed).map(|item| (indent, ListKind::Ordered, item))
  }
}

fn list_block(lines: &[&str], start: usize, depth: u32) -> (usize, String) {
  let (base_indent, kind, _) = list_item(lines[start]).expect("list_block starts on a recognized list item");
  let (open, close) = match kind {
    ListKind::Unordered => ("<ul>", "</ul>"),
    ListKind::Ordered => ("<ol>", "</ol>"),
  };
  let mut out = String::from(open);
  let mut i = start;
  while i < lines.len() {
    let Some((indent, item_kind, item)) = list_item(lines[i]) else { break };
    if indent != base_indent || item_kind != kind {
      break;
    }
    out.push_str(&format!("<li>{}", inline(item, depth)));
    i += 1;
    while i < lines.len() && depth < MAX_DEPTH {
      let Some((nested_indent, _, _)) = list_item(lines[i]) else { break };
      if nested_indent <= base_indent {
        break;
      }
      let (next, nested) = list_block(lines, i, depth + 1);
      out.push_str(&nested);
      i = next;
    }
    out.push_str("</li>");
  }
  out.push_str(close);
  (i, out)
}

/// Render one HTML list per source item. Repeated list containers are
/// deliberate: the page wraps each result in a separate comment target.
fn list_review_blocks(lines: &[&str], start: usize, depth: u32) -> (usize, Vec<(usize, String)>) {
  let (base_indent, _, _) = list_item(lines[start]).expect("review list starts on a recognized list item");
  let mut out = Vec::new();
  let mut i = start;
  let mut indents = vec![base_indent];
  while i < lines.len() {
    let Some((indent, item_kind, item)) = list_item(lines[i]) else { break };
    if indent < base_indent {
      break;
    }
    while indents.last().is_some_and(|current| *current > indent) {
      indents.pop();
    }
    if indents.last().is_none_or(|current| *current < indent) {
      indents.push(indent);
    }
    let nesting = indents.len() - 1;
    let (tag, start_attr) = match item_kind {
      ListKind::Unordered => ("ul", String::new()),
      ListKind::Ordered => {
        let trimmed = lines[i].trim_start_matches([' ', '\t']);
        let digits = trimmed.bytes().take_while(|byte| byte.is_ascii_digit()).count();
        let ordinal = trimmed[..digits].parse::<usize>().unwrap_or(1);
        ("ol", format!(r#" start="{ordinal}""#))
      }
    };
    let offset = nesting * 2;
    out.push((
      i,
      format!(
        r#"<{tag} class="md-list-fragment" style="--md-list-offset:{offset}em"{start_attr}><li>{}</li></{tag}>"#,
        inline(item, depth + nesting as u32)
      ),
    ));
    i += 1;
  }
  (i, out)
}

/// Emphasis content must be non-empty and not whitespace-flanked, so a bare
/// asterisk in prose (`a * b`) stays literal.
fn emphasizable(inner: &str) -> bool {
  !inner.is_empty() && inner.trim() == inner
}

/// True for link targets this module will emit as `href`.
fn safe_url(url: &str) -> bool {
  url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")
}

/// Inline spans: `` `code` `` (wins over everything inside it), `**bold**`,
/// `*italic*`, `[label](url)`. Anything unmatched stays escaped literal text.
fn inline(text: &str, depth: u32) -> String {
  if depth > MAX_DEPTH {
    return esc(text);
  }
  let mut out = String::new();
  let mut i = 0;
  while i < text.len() {
    let rest = &text[i..];
    if let Some(after) = rest.strip_prefix('`') {
      if let Some(n) = after.find('`') {
        out.push_str(&format!("<code>{}</code>", esc(&after[..n])));
        i += n + 2;
        continue;
      }
    }
    if let Some(after) = rest.strip_prefix("**") {
      if let Some(mut n) = after.find("**") {
        // `**outer *inner***`: the first `**` found sits inside the trailing
        // `***`; shift by one so the inner `*…*` pair stays intact.
        if after[n..].starts_with("***") {
          n += 1;
        }
        if emphasizable(&after[..n]) {
          out.push_str(&format!("<strong>{}</strong>", inline(&after[..n], depth + 1)));
          i += n + 4;
          continue;
        }
      }
    }
    if let Some(after) = rest.strip_prefix('*') {
      if let Some(n) = after.find('*') {
        if emphasizable(&after[..n]) {
          out.push_str(&format!("<em>{}</em>", inline(&after[..n], depth + 1)));
          i += n + 2;
          continue;
        }
      }
    }
    if rest.starts_with('[') {
      if let Some(close) = rest.find("](") {
        if let Some(end) = rest[close + 2..].find(')') {
          let label = &rest[1..close];
          let url = &rest[close + 2..close + 2 + end];
          if safe_url(url) {
            out.push_str(&format!(r#"<a href="{}">{}</a>"#, esc(url), inline(label, depth + 1)));
            i += close + 2 + end + 1;
            continue;
          }
        }
      }
    }
    // `text[i..]` is always on a char boundary: every arm advances by whole
    // characters (ASCII markers or a full `len_utf8`).
    let ch = rest.chars().next().expect("rest is non-empty inside the loop");
    esc_char(ch, &mut out);
    i += ch.len_utf8();
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn hostile_html_is_escaped_everywhere() {
    assert_eq!(to_html("<script>alert(1)</script>"), "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>");
    assert_eq!(to_html("# <b>hi</b>"), "<h1>&lt;b&gt;hi&lt;/b&gt;</h1>");
    assert_eq!(to_html("```\n<script>x</script>\n```"), "<pre><code>&lt;script&gt;x&lt;/script&gt;\n</code></pre>");
    assert_eq!(to_html("`<i>`"), "<p><code>&lt;i&gt;</code></p>");
  }

  #[test]
  fn unsafe_link_schemes_stay_literal_text() {
    let js = to_html("[x](javascript:alert(1))");
    assert!(!js.contains("<a "), "{js}");
    assert!(js.contains("javascript:alert(1)"));
    let ok = to_html("[docs](https://example.com/a?b=1)");
    assert_eq!(ok, r#"<p><a href="https://example.com/a?b=1">docs</a></p>"#);
  }

  #[test]
  fn headings_paragraphs_and_hard_breaks() {
    assert_eq!(to_html("## Title"), "<h2>Title</h2>");
    assert_eq!(to_html("####### seven"), "<p>####### seven</p>");
    assert_eq!(to_html("line one\nline two"), "<p>line one<br>line two</p>");
    assert_eq!(to_html("para one\n\npara two"), "<p>para one</p><p>para two</p>");
  }

  #[test]
  fn emphasis_code_and_snake_case_survival() {
    assert_eq!(
      to_html("**bold** and *em* and `code`"),
      "<p><strong>bold</strong> and <em>em</em> and <code>code</code></p>"
    );
    assert_eq!(to_html("**outer *inner***"), "<p><strong>outer <em>inner</em></strong></p>");
    assert_eq!(to_html("keep snake_case and __this__ literal"), "<p>keep snake_case and __this__ literal</p>");
    assert_eq!(to_html("a * b stays literal"), "<p>a * b stays literal</p>");
  }

  #[test]
  fn lists_blockquotes_and_rules() {
    assert_eq!(to_html("- a\n- b"), "<ul><li>a</li><li>b</li></ul>");
    assert_eq!(to_html("1. a\n2. b"), "<ol><li>a</li><li>b</li></ol>");
    assert_eq!(to_html("> quoted\n> more"), "<blockquote><p>quoted<br>more</p></blockquote>");
    assert_eq!(to_html("---"), "<hr>");
  }

  #[test]
  fn nested_lists_preserve_source_indentation() {
    assert_eq!(
      to_html("- parent\n  - child\n    - grandchild\n- sibling"),
      "<ul><li>parent<ul><li>child<ul><li>grandchild</li></ul></li></ul></li><li>sibling</li></ul>"
    );
    assert_eq!(
      to_html("1. parent\n   - child\n2. sibling"),
      "<ol><li>parent<ul><li>child</li></ul></li><li>sibling</li></ol>"
    );
  }

  #[test]
  fn unclosed_fence_runs_to_the_end() {
    assert_eq!(to_html("```\ncode"), "<pre><code>code\n</code></pre>");
  }

  #[test]
  fn multibyte_text_is_preserved() {
    assert_eq!(to_html("héllo — **wörld** 🚀"), "<p>héllo — <strong>wörld</strong> 🚀</p>");
  }

  #[test]
  fn blocks_carry_their_starting_source_lines() {
    // The fence spans a blank line (9-13), so naive blank-line splitting
    // would mangle it; block tracking must not.
    let text = "# Title\n\npara one\npara two\n\n- a\n- b\n\n```\ncode\n\nmore\n```\n\n> q";
    let blocks = to_html_blocks(text);
    let lines: Vec<usize> = blocks.iter().map(|(l, _)| *l).collect();
    assert_eq!(lines, vec![0, 2, 5, 8, 14]);
    assert!(blocks[0].1.starts_with("<h1>"), "{}", blocks[0].1);
    assert!(blocks[3].1.starts_with("<pre>"), "{}", blocks[3].1);
  }

  #[test]
  fn review_blocks_give_every_list_item_its_source_line() {
    let blocks = to_html_review_blocks("# Title\n\n- parent\n  - child\n- sibling\n\n1. first\n2. second");
    let lines: Vec<usize> = blocks.iter().map(|(line, _)| *line).collect();
    assert_eq!(lines, vec![0, 2, 3, 4, 6, 7]);
    assert_eq!(blocks[1].1, r#"<ul class="md-list-fragment" style="--md-list-offset:0em"><li>parent</li></ul>"#);
    assert_eq!(blocks[2].1, r#"<ul class="md-list-fragment" style="--md-list-offset:2em"><li>child</li></ul>"#);
    assert!(blocks[4].1.starts_with(r#"<ol class="md-list-fragment" style="--md-list-offset:0em" start="1">"#));
    assert!(blocks[5].1.starts_with(r#"<ol class="md-list-fragment" style="--md-list-offset:0em" start="2">"#));
  }

  #[test]
  fn concatenated_blocks_equal_to_html() {
    for text in [
      "# Title\n\npara one\npara two\n\n- a\n- b\n\n```\ncode\n\nmore\n```\n\n> q\n\n---",
      "plain",
      "",
      "> nested\n> > deeper\n\n1. one\n2. two",
    ] {
      let joined: String = to_html_blocks(text).into_iter().map(|(_, h)| h).collect();
      assert_eq!(joined, to_html(text), "for input {text:?}");
    }
  }

  #[test]
  fn pipe_tables_render_with_alignment_padding_and_escaped_cells() {
    let md = "Coverage:\n| Behavior | Coverage | Note |\n|:---|:-:|--:|\n| `a \\| b` | unit ✓ |\n| <b>x</b> | **none** | extra | dropped |\n\nAfter.";
    let html = to_html(md);
    assert_eq!(
      html,
      "<p>Coverage:</p>\
<table><thead><tr><th style=\"text-align:left\">Behavior</th><th style=\"text-align:center\">Coverage</th><th style=\"text-align:right\">Note</th></tr></thead>\
<tbody><tr><td style=\"text-align:left\"><code>a | b</code></td><td style=\"text-align:center\">unit ✓</td><td style=\"text-align:right\"></td></tr>\
<tr><td style=\"text-align:left\">&lt;b&gt;x&lt;/b&gt;</td><td style=\"text-align:center\"><strong>none</strong></td><td style=\"text-align:right\">extra</td></tr></tbody></table>\
<p>After.</p>"
    );
  }

  #[test]
  fn a_table_needs_its_delimiter_row_and_ends_where_the_pipes_do() {
    assert_eq!(to_html("a | b\nc | d"), "<p>a | b<br>c | d</p>", "no delimiter row: a paragraph");
    assert_eq!(to_html("| a | b |\n|---|"), "<p>| a | b |<br>|---|</p>", "width mismatch: a paragraph");
    assert_eq!(
      to_html("| a |\n|---|"),
      "<table><thead><tr><th>a</th></tr></thead></table>",
      "a header alone is a table"
    );
    assert_eq!(
      to_html("| a |\n|---|\n| 1 |\nplain\n| 2 |"),
      "<table><thead><tr><th>a</th></tr></thead><tbody><tr><td>1</td></tr></tbody></table><p>plain<br>| 2 |</p>"
    );
    // A table's blocks anchor to their own source line, like every other block.
    let blocks = to_html_blocks("intro\n\n| a |\n|---|\n| 1 |\n\ntail");
    assert_eq!(blocks.iter().map(|(line, _)| *line).collect::<Vec<_>>(), vec![0, 2, 6]);
  }
}
