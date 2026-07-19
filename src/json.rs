//! A tiny, std-only JSON reader — just enough to pull a human-readable message out
//! of a skill's result file. The root crate carries no serde, so (like the YAML
//! subset in [`crate::config`]) this is a small purpose-built parser, not a library.

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
  Null,
  Bool(bool),
  Number(f64),
  String(String),
  Array(Vec<Value>),
  Object(Vec<(String, Value)>),
}

/// Parse a complete JSON document, or return a short reason on malformed input.
pub fn parse(text: &str) -> Result<Value, String> {
  let mut p = Parser { b: text.as_bytes(), i: 0 };
  let v = p.value()?;
  p.ws();
  if p.i != p.b.len() {
    return Err("trailing characters after the JSON value".into());
  }
  Ok(v)
}

/// The best human-readable message from a skill's result file: scsh parses the file
/// as JSON and, when it is an object, returns the `result` field, else `message`,
/// else the value of a lone single field (so `{"ab": "…"}` works too). A structured
/// `result` OBJECT (the code-review skills write `result: {grade, issues_found}`)
/// becomes a compact glimpse of its scalar fields — `grade: excellent · issues_found: 3`.
/// `None` when there is no obvious message — the caller then falls back to the file path.
pub fn message(text: &str) -> Option<String> {
  let obj = match parse(text).ok()? {
    Value::Object(o) => o,
    _ => return None,
  };
  for key in ["result", "message"] {
    match obj.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
      Some(Value::String(s)) => return Some(s.clone()),
      Some(Value::Object(fields)) if key == "result" => {
        if let Some(glimpse) = scalar_glimpse(fields) {
          return Some(glimpse);
        }
      }
      _ => {}
    }
  }
  if obj.len() == 1 {
    if let (_, Value::String(s)) = &obj[0] {
      return Some(s.clone());
    }
  }
  None
}

/// `key: value` for each scalar field of an object, joined with ` · ` — the glimpse a
/// structured result shows on a skill's summary line. Arrays and nested objects are
/// skipped; `None` when nothing scalar remains.
fn scalar_glimpse(fields: &[(String, Value)]) -> Option<String> {
  let parts: Vec<String> = fields
    .iter()
    .filter_map(|(k, v)| {
      let shown = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 => format!("{}", *n as i64),
        Value::Number(n) if n.is_finite() => format!("{n}"),
        Value::Bool(b) => b.to_string(),
        _ => return None,
      };
      Some(format!("{k}: {shown}"))
    })
    .collect();
  if parts.is_empty() {
    None
  } else {
    Some(parts.join(" · "))
  }
}

/// The string value of a top-level object field `key`, if present and a string. Used to
/// read a cache entry's `result`. `None` if the text isn't an object with that key.
pub fn field(text: &str, key: &str) -> Option<String> {
  match parse(text).ok()? {
    Value::Object(o) => o.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
      Value::String(s) => Some(s.clone()),
      _ => None,
    }),
    _ => None,
  }
}

/// A JSON string literal (quoted and escaped) — for writing cache entries without a
/// serialization crate. The inverse of what [`Parser::string`] reads.
pub fn quote(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 2);
  out.push('"');
  for c in s.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
      c => out.push(c),
    }
  }
  out.push('"');
  out
}

/// Serialize a [`Value`] back to compact JSON — the inverse of [`parse`]. Whole numbers
/// print without a fractional part (config counters would change meaning as `1.0`).
pub fn write(v: &Value) -> String {
  match v {
    Value::Null => "null".into(),
    Value::Bool(b) => b.to_string(),
    Value::Number(n) if n.fract() == 0.0 && n.is_finite() && n.abs() < 9.0e15 => format!("{}", *n as i64),
    Value::Number(n) => format!("{n}"),
    Value::String(s) => quote(s),
    Value::Array(a) => format!("[{}]", a.iter().map(write).collect::<Vec<_>>().join(",")),
    Value::Object(o) => {
      let fields: Vec<String> = o.iter().map(|(k, v)| format!("{}:{}", quote(k), write(v))).collect();
      format!("{{{}}}", fields.join(","))
    }
  }
}

/// Serialize a [`Value`] as human-readable two-space-indented JSON — the canonical on-disk
/// form for stored results. Same value semantics as [`write`]; only whitespace differs.
/// [`quote`] passes non-ASCII through unescaped, so round-tripping an agent's result through
/// this writer also normalizes any \uXXXX escape noise into readable text.
pub fn write_pretty(v: &Value) -> String {
  fn go(v: &Value, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let inner = "  ".repeat(indent + 1);
    match v {
      Value::Array(a) if !a.is_empty() => {
        let items: Vec<String> = a.iter().map(|x| format!("{inner}{}", go(x, indent + 1))).collect();
        format!("[\n{}\n{pad}]", items.join(",\n"))
      }
      Value::Object(o) if !o.is_empty() => {
        let fields: Vec<String> =
          o.iter().map(|(k, x)| format!("{inner}{}: {}", quote(k), go(x, indent + 1))).collect();
        format!("{{\n{}\n{pad}}}", fields.join(",\n"))
      }
      other => write(other),
    }
  }
  format!("{}\n", go(v, 0))
}

struct Parser<'a> {
  b: &'a [u8],
  i: usize,
}

impl Parser<'_> {
  fn ws(&mut self) {
    while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
      self.i += 1;
    }
  }

  fn value(&mut self) -> Result<Value, String> {
    self.ws();
    match self.b.get(self.i) {
      Some(b'{') => self.object(),
      Some(b'[') => self.array(),
      Some(b'"') => Ok(Value::String(self.string()?)),
      Some(b't') => self.lit("true", Value::Bool(true)),
      Some(b'f') => self.lit("false", Value::Bool(false)),
      Some(b'n') => self.lit("null", Value::Null),
      Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
      _ => Err("expected a JSON value".into()),
    }
  }

  fn lit(&mut self, word: &str, v: Value) -> Result<Value, String> {
    if self.b[self.i..].starts_with(word.as_bytes()) {
      self.i += word.len();
      Ok(v)
    } else {
      Err(format!("invalid literal (expected {word})"))
    }
  }

  fn object(&mut self) -> Result<Value, String> {
    self.i += 1; // consume '{'
    let mut out = Vec::new();
    self.ws();
    if self.b.get(self.i) == Some(&b'}') {
      self.i += 1;
      return Ok(Value::Object(out));
    }
    loop {
      self.ws();
      if self.b.get(self.i) != Some(&b'"') {
        return Err("expected a string key".into());
      }
      let key = self.string()?;
      self.ws();
      if self.b.get(self.i) != Some(&b':') {
        return Err("expected ':' after key".into());
      }
      self.i += 1;
      out.push((key, self.value()?));
      self.ws();
      match self.b.get(self.i) {
        Some(b',') => self.i += 1,
        Some(b'}') => {
          self.i += 1;
          return Ok(Value::Object(out));
        }
        _ => return Err("expected ',' or '}' in object".into()),
      }
    }
  }

  fn array(&mut self) -> Result<Value, String> {
    self.i += 1; // consume '['
    let mut out = Vec::new();
    self.ws();
    if self.b.get(self.i) == Some(&b']') {
      self.i += 1;
      return Ok(Value::Array(out));
    }
    loop {
      out.push(self.value()?);
      self.ws();
      match self.b.get(self.i) {
        Some(b',') => self.i += 1,
        Some(b']') => {
          self.i += 1;
          return Ok(Value::Array(out));
        }
        _ => return Err("expected ',' or ']' in array".into()),
      }
    }
  }

  fn string(&mut self) -> Result<String, String> {
    self.i += 1; // consume opening '"'
    let mut s = String::new();
    while let Some(&c) = self.b.get(self.i) {
      match c {
        b'"' => {
          self.i += 1;
          return Ok(s);
        }
        b'\\' => {
          self.i += 1;
          match self.b.get(self.i) {
            Some(b'"') => s.push('"'),
            Some(b'\\') => s.push('\\'),
            Some(b'/') => s.push('/'),
            Some(b'n') => s.push('\n'),
            Some(b't') => s.push('\t'),
            Some(b'r') => s.push('\r'),
            Some(b'b') => s.push('\u{8}'),
            Some(b'f') => s.push('\u{c}'),
            Some(b'u') => {
              let hex = self.b.get(self.i + 1..self.i + 5).ok_or("truncated \\u escape")?;
              let cp = u32::from_str_radix(std::str::from_utf8(hex).map_err(|_| "bad \\u escape")?, 16)
                .map_err(|_| "bad \\u escape")?;
              s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
              self.i += 4;
            }
            _ => return Err("bad escape in string".into()),
          }
          self.i += 1;
        }
        _ => {
          let len = utf8_len(c);
          let chunk = self.b.get(self.i..self.i + len).ok_or("invalid UTF-8 in string")?;
          s.push_str(std::str::from_utf8(chunk).map_err(|_| "invalid UTF-8 in string")?);
          self.i += len;
        }
      }
    }
    Err("unterminated string".into())
  }

  fn number(&mut self) -> Result<Value, String> {
    let start = self.i;
    while let Some(&c) = self.b.get(self.i) {
      if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E') {
        self.i += 1;
      } else {
        break;
      }
    }
    let text = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad number")?;
    text.parse::<f64>().map(Value::Number).map_err(|_| format!("bad number '{text}'"))
  }
}

/// Byte length of a UTF-8 char from its leading byte.
fn utf8_len(first: u8) -> usize {
  match first {
    b if b < 0x80 => 1,
    b if b >> 5 == 0b110 => 2,
    b if b >> 4 == 0b1110 => 3,
    _ => 4,
  }
}

#[cfg(test)]
mod tests {
  #[test]
  fn write_pretty_indents_and_keeps_non_ascii_readable() {
    let v = parse(r#"{"a":[1,2],"b":{"c":"\u201cquoted\u201d"},"empty":[],"none":{}}"#).unwrap();
    let pretty = write_pretty(&v);
    assert_eq!(
      pretty,
      "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {\n    \"c\": \"\u{201c}quoted\u{201d}\"\n  },\n  \"empty\": [],\n  \"none\": {}\n}\n"
    );
    assert_eq!(write(&parse(&pretty).unwrap()), write(&v), "pretty output round-trips to the same value");
  }

  use super::*;

  #[test]
  fn parses_scalars_arrays_and_objects() {
    assert_eq!(parse("null").unwrap(), Value::Null);
    assert_eq!(parse(" true ").unwrap(), Value::Bool(true));
    assert_eq!(parse("-12.5e1").unwrap(), Value::Number(-125.0));
    assert_eq!(parse(r#""hi\n\"x\"""#).unwrap(), Value::String("hi\n\"x\"".into()));
    assert_eq!(parse("[1, 2]").unwrap(), Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]));
    assert_eq!(
      parse(r#"{"a": "b", "n": 1}"#).unwrap(),
      Value::Object(vec![("a".into(), Value::String("b".into())), ("n".into(), Value::Number(1.0))])
    );
  }

  #[test]
  fn rejects_malformed_json() {
    assert!(parse("{").is_err());
    assert!(parse(r#"{"a": }"#).is_err());
    assert!(parse("[1, 2").is_err());
    assert!(parse(r#""unterminated"#).is_err());
    assert!(parse("nul").is_err());
    assert!(parse(r#"{"a":1} junk"#).is_err());
  }

  #[test]
  fn message_prefers_result_then_message_then_single_field() {
    // Explicit `result` field wins.
    assert_eq!(message(r#"{"result": "done", "other": 1}"#).as_deref(), Some("done"));
    // Then `message`.
    assert_eq!(message(r#"{"message": "hi", "x": 2}"#).as_deref(), Some("hi"));
    // A lone single string field (covers the example skills' {"ab": "..."}).
    assert_eq!(message(r#"{"ab": "the sum is eight"}"#).as_deref(), Some("the sum is eight"));
    // Unicode-escaped and multi-line values survive.
    assert_eq!(message(r#"{"pq": "two\n(note: defaulted)"}"#).as_deref(), Some("two\n(note: defaulted)"));
  }

  #[test]
  fn field_reads_a_string_value_and_quote_roundtrips() {
    assert_eq!(field(r#"{"result": "2 + 3 = 5", "n": 1}"#, "result").as_deref(), Some("2 + 3 = 5"));
    assert_eq!(field(r#"{"result": 5}"#, "result"), None); // not a string
    assert_eq!(field(r#"{"a": "b"}"#, "result"), None); // absent
                                                        // quote() escapes so the result round-trips back through the parser.
    let raw = "line1\n\"quoted\"\tend\\x";
    let entry = format!("{{\"result\": {}}}", quote(raw));
    assert_eq!(field(&entry, "result").as_deref(), Some(raw));
  }

  #[test]
  fn message_is_none_without_an_obvious_field() {
    assert_eq!(message(r#"{"a": 1, "b": 2}"#), None); // two fields, neither a string key we know
    assert_eq!(message("[1, 2, 3]"), None); // not an object
    assert_eq!(message("not json"), None); // unparseable
    assert_eq!(message(r#"{"a": "x", "b": "y"}"#), None); // ambiguous: two string fields, no result/message
  }

  #[test]
  fn message_composes_a_glimpse_for_an_object_result() {
    // The code-review skills' documented shape: `result` is an object of scalars.
    assert_eq!(
      message(r#"{"result": {"grade": "excellent", "issues_found": 3}, "issues": []}"#).as_deref(),
      Some("grade: excellent · issues_found: 3")
    );
    // Non-scalar fields are skipped; the remaining scalars still make a glimpse.
    assert_eq!(message(r#"{"result": {"grade": "good", "parts": [1, 2]}}"#).as_deref(), Some("grade: good"));
    // Booleans and non-integer numbers render plainly.
    assert_eq!(message(r#"{"result": {"passed": true, "score": 4.5}}"#).as_deref(), Some("passed: true · score: 4.5"));
    // An object result with nothing scalar is no glimpse at all.
    assert_eq!(message(r#"{"result": {"parts": [1]}}"#), None);
    // A string `result` still wins, unchanged.
    assert_eq!(message(r#"{"result": "done", "grade": "poor"}"#).as_deref(), Some("done"));
    // An object `result` does not shadow a string `message`.
    assert_eq!(message(r#"{"result": {"parts": [1]}, "message": "hi"}"#).as_deref(), Some("hi"));
  }
}
