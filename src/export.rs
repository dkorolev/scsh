//! Cast export: render a recorded asciicast — plus its `.chapters.json` annotation sidecar,
//! when one sits next to it — into ONE self-contained offline `.html` player page.
//!
//! The page pipeline itself (template, the embedded first-party player, strict `<script>`-safe
//! escaping) is `beecast-page`, a deliberate zero-transitive-dependency crate; this module is
//! the pure glue: find + parse the sidecar (reusing [`crate::annotate`]'s parser — the sidecar
//! IS that module's output format), map it onto beecast's [`PageMeta`], and render. All of it
//! is side-effect-free except [`load_sidecar`]'s single file read, so the mapping is unit-testable.

use std::path::{Path, PathBuf};

use beecast_page::{build_page, inspect, CastError, PageMeta};

use crate::annotate::CastAnnotation;

/// The annotation sidecar next to a cast, as found on disk.
#[derive(Debug)]
pub enum Sidecar {
  /// No `<stem>.chapters.json` next to the cast (or the cast is not `.cast`-named).
  Absent,
  /// A sidecar that parsed into a summary + chapters.
  Found(CastAnnotation),
  /// A sidecar exists but is not valid `{summary, chapters}` JSON — a warning, never a
  /// failure: the export proceeds without it. Carries the offending path for the message.
  Malformed(PathBuf),
}

/// Locate and parse the cast's `.chapters.json` sidecar. Reuses [`crate::annotate`]'s
/// parser (`parse_annotation`) rather than duplicating one — the sidecar is exactly what
/// `annotate-cast` writes, `{summary, chapters: [{t, title}]}`.
pub fn load_sidecar(cast_path: &Path) -> Sidecar {
  let Some(sidecar) = crate::daemon::chapters_sidecar_path(&cast_path.to_string_lossy()) else {
    return Sidecar::Absent;
  };
  let Ok(text) = std::fs::read_to_string(&sidecar) else { return Sidecar::Absent };
  match crate::annotate::parse_annotation(&text) {
    Some(a) => Sidecar::Found(a),
    None => Sidecar::Malformed(sidecar),
  }
}

/// The page title for a cast: its file stem (`…/foo.cast` → `foo`), falling back to the
/// whole path when there is no file name to speak of.
pub fn cast_stem(cast_path: &Path) -> String {
  match cast_path.file_stem() {
    Some(stem) => stem.to_string_lossy().into_owned(),
    None => cast_path.to_string_lossy().into_owned(),
  }
}

/// The default output path: `<stem>.html` next to the cast, matching the sidecar's
/// `<stem>.chapters.json` convention. An input not named `*.cast` just gains `.html`.
pub fn default_output_path(cast_path: &Path) -> PathBuf {
  match cast_path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".cast")) {
    Some(stem) => cast_path.with_file_name(format!("{stem}.html")),
    None => PathBuf::from(format!("{}.html", cast_path.display())),
  }
}

/// Render the page: `inspect` the recording first (a non-asciicast is an error the caller
/// turns into an actionable `✗`/`→`), then map the sidecar annotation onto beecast's
/// [`PageMeta`] — the cast's stem is always the title; the sidecar, when present,
/// contributes the summary and the `(t, title)` chapter markers.
pub fn render_page(cast_ndjson: &str, stem: &str, annotation: Option<&CastAnnotation>) -> Result<String, CastError> {
  inspect(cast_ndjson)?;
  let chapters: Vec<(f64, &str)> =
    annotation.map(|a| a.chapters.iter().map(|c| (c.t, c.title.as_str())).collect()).unwrap_or_default();
  let meta = PageMeta { title: Some(stem), summary: annotation.map(|a| a.summary.as_str()), chapters: &chapters };
  Ok(build_page(cast_ndjson, &meta, stem))
}

/// [`render_page`] from raw texts, for callers holding the sidecar's bytes rather than a
/// parsed [`CastAnnotation`] (the daemon's `/export.html` endpoint). An absent or malformed
/// sidecar exports without summary/chapters — the same never-a-failure behavior as the CLI.
pub fn render_page_from_texts(cast_ndjson: &str, sidecar_json: Option<&str>, stem: &str) -> Result<String, CastError> {
  let annotation = sidecar_json.and_then(crate::annotate::parse_annotation);
  render_page(cast_ndjson, stem, annotation.as_ref())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::annotate::Chapter;

  const CAST: &str =
    "{\"version\":3,\"term\":{\"cols\":80,\"rows\":24}}\n[0.5,\"o\",\"hello\\r\\n\"]\n[1.5,\"o\",\"done\\r\\n\"]\n";

  fn annotation() -> CastAnnotation {
    CastAnnotation {
      summary: "Ran the demo.".into(),
      chapters: vec![Chapter { t: 0.0, title: "Start".into() }, Chapter { t: 1.5, title: "Finish".into() }],
    }
  }

  #[test]
  fn render_page_maps_the_sidecar_onto_page_meta() {
    let a = annotation();
    let page = render_page(CAST, "rec-042", Some(&a)).unwrap();
    assert!(page.contains("<title>rec-042</title>"), "stem is the title; got no such <title>");
    assert!(page.contains("Ran the demo."), "summary rendered");
    assert!(page.contains("\"chapters\":[{\"t\":0.0,\"title\":\"Start\"},{\"t\":1.5,\"title\":\"Finish\"}]"));
  }

  #[test]
  fn render_page_without_a_sidecar_falls_back_to_the_stem_title() {
    let page = render_page(CAST, "bare-rec", None).unwrap();
    assert!(page.contains("<title>bare-rec</title>"));
    assert!(page.contains("<p id=\"summary\" hidden></p>"), "no summary stays hidden");
    assert!(!page.contains("\"chapters\":["), "no chapters without a sidecar");
  }

  #[test]
  fn render_page_rejects_a_non_asciicast() {
    assert_eq!(render_page("not a recording at all", "x", None), Err(CastError::NotAsciicast));
  }

  #[test]
  fn render_page_from_texts_parses_the_sidecar_and_shrugs_off_junk() {
    // A valid sidecar text contributes its chapters; junk (or none) exports chapterless.
    let page = render_page_from_texts(CAST, Some(&annotation().to_sidecar_json()), "rec-042").unwrap();
    assert!(page.contains("\"chapters\":[{\"t\":0.0,\"title\":\"Start\"},{\"t\":1.5,\"title\":\"Finish\"}]"));
    for sidecar in [Some("{ not json"), None] {
      let page = render_page_from_texts(CAST, sidecar, "rec-042").unwrap();
      assert!(page.contains("<title>rec-042</title>"));
      assert!(!page.contains("\"chapters\":["), "malformed/absent sidecar exports without chapters");
    }
  }

  #[test]
  fn load_sidecar_reports_absent_found_and_malformed() {
    let dir = std::env::temp_dir().join(format!("scsh-export-test-{}", crate::runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("rec.cast");
    std::fs::write(&cast, CAST).unwrap();
    // No sidecar on disk yet → Absent.
    assert!(matches!(load_sidecar(&cast), Sidecar::Absent));
    // A valid `annotate-cast` sidecar → Found, with the summary and chapters intact.
    std::fs::write(dir.join("rec.chapters.json"), annotation().to_sidecar_json()).unwrap();
    match load_sidecar(&cast) {
      Sidecar::Found(a) => {
        assert_eq!(a.summary, "Ran the demo.");
        assert_eq!(a.chapters.len(), 2);
      }
      other => panic!("expected Found, got {other:?}"),
    }
    // Junk in the sidecar → Malformed (the warning path), naming the sidecar file.
    std::fs::write(dir.join("rec.chapters.json"), "{ not json").unwrap();
    match load_sidecar(&cast) {
      Sidecar::Malformed(p) => assert!(p.ends_with("rec.chapters.json"), "got: {}", p.display()),
      other => panic!("expected Malformed, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
  }

  #[test]
  fn default_output_path_sits_next_to_the_cast() {
    assert_eq!(default_output_path(Path::new("/a/b/foo.cast")), PathBuf::from("/a/b/foo.html"));
    // A recording not named `*.cast` still gets a sensible page path.
    assert_eq!(default_output_path(Path::new("/a/b/session.rec")), PathBuf::from("/a/b/session.rec.html"));
  }
}
