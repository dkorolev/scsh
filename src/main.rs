//! scsh — Scoped Skills Helper.
//!
//! Preflight a git repository (git → repo → `.scsh.yml` present → schema-valid →
//! a container runtime), then build one in-memory image and run the project's
//! scoped skills — all of them, in parallel, each in its own ephemeral container
//! under its configured harness.

mod annotate;
mod config;
mod daemon;
mod export;
mod failure;
mod fleet;
mod gc;
mod harness_def;
mod json;
#[cfg(test)]
mod licenses;
mod ptyrec;
mod runtime;
mod sha1;
mod sha256;
mod stats;
mod ui;
mod version;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use config::ResolvedInvocation;
use runtime::Runtime;

fn fleet_route_name(skill: &ResolvedInvocation) -> Option<&str> {
  fleet::route_name(&skill.name, &skill.skill_source)
}

fn main() {
  let args: Vec<String> = std::env::args().skip(1).collect();
  std::process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
  let cli = match parse_cli(args) {
    Ok(c) => c,
    Err(e) => {
      eprintln!("scsh: {e}");
      eprintln!("try 'scsh --help'");
      return 2;
    }
  };
  let profile = cli.profile.as_deref();
  if let Some(n) = cli.retries {
    // One process-wide answer: the daemon client reads SCSH_RETRIES at registration.
    std::env::set_var("SCSH_RETRIES", n.to_string());
  }
  match cli.mode {
    Mode::Help(topic) => {
      print_help(topic);
      0
    }
    Mode::Version => {
      println!("scsh {}", version_id());
      0
    }
    Mode::InitDemo => init_demo(),
    Mode::Demo { name } => demo_cmd(name.as_deref()),
    Mode::InstallSkills => install_skills(false, &cli.sources, cli.global),
    Mode::UpdateSkills => install_skills(true, &cli.sources, cli.global),
    Mode::List => {
      // `--json` is a runtime-free, machine-readable listing (just git + a valid .scsh.yml);
      // the human listing goes through the full preflight like a run does. NOTE: unlike the
      // §2 ideal, `list` does NOT auto-switch to JSON when piped — its human output (result
      // paths, `--verbose` build commands) is not a subset of the JSON, and existing docs and
      // scripts grep that human text. Machines opt in explicitly with `--json`.
      if cli.json {
        list_profiles_json(cli.override_dot_scsh_yml.as_deref())
      } else {
        preflight_then(Action::List, profile, cli.verbose, cli.override_dot_scsh_yml.as_deref(), None)
      }
    }
    Mode::CheckProfile => check_profile_cmd(profile, cli.override_dot_scsh_yml.as_deref()),
    Mode::Probe => probe_cmd(profile, cli.override_dot_scsh_yml.as_deref(), cli.json),
    Mode::Run => match cli.def.as_deref() {
      // The daemon's "start a job" passes the session id it pre-created so its deep link and
      // the one-job-per-repo guard are authoritative before this child registers.
      Some(name) => {
        preflight_then_def(name, cli.failures.session.as_deref(), cli.resume_from.as_deref(), cli.base.as_deref())
      }
      None => {
        preflight_then(Action::Run, profile, cli.verbose, cli.override_dot_scsh_yml.as_deref(), cli.base.as_deref())
      }
    },
    // Hidden: a self-contained demo of the live board (no container/model needed), used by the
    // feature's demo + PTY test. `--frames` dumps deterministic plain frames; otherwise it runs
    // the real interactive board over a few scripted subprocesses.
    Mode::UiDemo { frames } => ui::demo::run(frames),
    Mode::Daemon { action } => daemon_cmd(action),
    Mode::DaemonServe { mode, port } => daemon_serve(mode, port),
    Mode::RecordPty { cast, cols, rows, argv } => ptyrec::record(&cast, cols, rows, &argv),
    Mode::Failures => failures_cmd(&cli.failures),
    Mode::Stats => stats_cmd(&cli.failures, profile),
    Mode::Prune => prune_cmd(cli.prune_now),
    Mode::Gc => gc_cmd(&cli.gc),
    Mode::AnnotateCasts => annotate_casts_cmd(&cli.annotate_paths, cli.json),
    Mode::ExportCasts => export_casts_cmd(&cli.export_paths, cli.output.as_deref(), cli.json),
    Mode::BuildImages => {
      build_images_cmd(&cli.build_harnesses, cli.build_force, cli.build_rebuild_base, cli.failures.session.clone())
    }
  }
}

/// The Codex model annotation uses, overridable via `SCSH_ANNOTATE_MODEL`.
fn annotate_model() -> String {
  std::env::var("SCSH_ANNOTATE_MODEL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "gpt-5.6-luna".into())
}

/// `annotate-cast <cast>…`: write a `{summary, chapters}` sidecar next to each cast using
/// Codex on the lightweight Luna route. Human output (progress, notes) goes to stderr; with
/// `--json` (or when stdout is not a TTY) the sidecar paths are emitted as JSON on stdout,
/// and errors as a single-key `{"Error": …}` object (§2).
///
/// When the session browser daemon is already up, registers a short `(internal)` session so
/// annotate progress is visible as [`daemon::ProcKind::Annotate`] rows (optional
/// `parent_session` when a cast path sits under `/sessions/<id>/`).
fn annotate_casts_cmd(paths: &[String], json_flag: bool) -> i32 {
  use std::io::IsTerminal;
  let as_json = json_flag || !std::io::stdout().is_terminal();
  if paths.is_empty() {
    return annotate_error(
      as_json,
      2,
      "give one or more .cast files, e.g. scsh annotate-cast ~/.scsh/sessions/<session>/casts/foo.cast",
    );
  }
  if !annotate::host_can_annotate() {
    return annotate_error(as_json, 1, "Codex not available (need the `codex` CLI and a Codex login)");
  }
  // An explicit retry is the user's override of an earlier browser cancellation. Automatic
  // children set SCSH_AUTO_ANNOTATE and continue to honor the durable suppression marker.
  if std::env::var_os("SCSH_AUTO_ANNOTATE").is_none() {
    for path in paths {
      let _ = std::fs::remove_file(annotate::suppression_marker(Path::new(path)));
    }
  }
  let model = annotate_model();
  let parent = paths.iter().find_map(|p| parent_session_from_cast_path(p));
  let daemon_session = if daemon::Client::daemon_alive() {
    let session_id = daemon::new_session_id();
    let client = daemon::Client::new(session_id);
    if client.register_session_with_workflow(
      daemon::INTERNAL_REPO,
      "",
      Some("annotate"),
      "annotate",
      &[],
      None,
      parent.as_deref(),
    ) {
      if !as_json {
        eprintln!("annotate: track progress at {}", client.session_url());
      }
      Some(client)
    } else {
      None
    }
  } else {
    None
  };
  // Annotation can be visually quiet while Codex thinks. Keep the daemon's 30-second
  // stale-session detector informed independently of terminal output.
  let heartbeat = daemon_session.as_ref().map(|c| c.start_heartbeat(Duration::from_secs(5)));
  let mut sidecars: Vec<(String, String)> = Vec::new(); // (cast, sidecar)
  let mut next_idx = 0usize;
  for path in paths {
    if !as_json {
      eprint!("annotate {path} … ");
    }
    let proc_idx = if daemon_session.is_some() {
      let idx = next_idx;
      next_idx += 1;
      let stem = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("cast");
      let mut label = format!("annotate · {stem}");
      if label.len() > 60 {
        label.truncate(57);
        label.push('…');
      }
      if let Some(c) = &daemon_session {
        // The annotated cast path rides along so the daemon can point the original job's
        // "chapters: summarizing…" note at THIS session while the annotation runs.
        c.proc_add(
          idx,
          &label,
          daemon::ProcKind::Annotate,
          None,
          Some("codex"),
          Some(model.as_str()),
          None,
          None,
          Some(path.as_str()),
          None,
        );
        c.proc_start(idx);
        c.proc_note(idx, "summarizing…");
      }
      Some(idx)
    } else {
      None
    };
    let started = std::time::Instant::now();
    // Only pre-register a recording when the host can actually produce one (tmux +
    // asciinema); otherwise the proc would carry a cast path that never materializes
    // and the job page would show an empty player instead of the text row.
    let record_cast = daemon_session.as_ref().filter(|_| annotate::can_record_annotate()).map(|c| {
      let casts = runtime::session_casts_dir(c.session_id());
      let _ = std::fs::create_dir_all(&casts);
      let stem = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("cast");
      casts.join(format!(
        "annotate-{}-{}-utc-{}.cast",
        stem.replace('/', "_"),
        runtime::format_utc_timestamp(now_secs()),
        runtime::random_nonce_6()
      ))
    });
    if let (Some(c), Some(idx), Some(cast)) = (&daemon_session, proc_idx, &record_cast) {
      c.proc_cast(idx, &cast.to_string_lossy());
    }
    match annotate::annotate_cast(std::path::Path::new(path), &model, record_cast.as_deref()) {
      Ok(result) => {
        if let (Some(c), Some(idx)) = (&daemon_session, proc_idx) {
          if let Some(ref cast) = result.cast_path {
            c.proc_cast(idx, &cast.to_string_lossy());
          }
          c.proc_finish(idx, daemon::ProcStatus::Ok, None, None, started.elapsed().as_secs_f64());
        }
        if !as_json {
          eprintln!("✓ {}", result.sidecar.display());
        }
        sidecars.push((path.clone(), result.sidecar.to_string_lossy().into_owned()));
      }
      Err(err) => {
        // The distinct reason (unreadable cast / empty transcript / model timeout / …)
        // lands both in the daemon Fail row and on stderr, with the paired `→` fix.
        if let (Some(c), Some(idx)) = (&daemon_session, proc_idx) {
          c.proc_finish(
            idx,
            daemon::ProcStatus::Fail,
            Some(err.failure_reason()),
            Some(&err.to_string()),
            started.elapsed().as_secs_f64(),
          );
        }
        if !as_json {
          eprintln!("✗ {err}");
          eprintln!("  → {}", err.hint());
        }
      }
    }
  }
  if let Some(flag) = heartbeat {
    flag.store(false, std::sync::atomic::Ordering::Relaxed);
  }
  if let Some(c) = daemon_session {
    c.finish_session();
  }
  if as_json {
    let items: Vec<String> = sidecars
      .iter()
      .map(|(cast, side)| format!("    {{ \"cast\": {}, \"sidecar\": {} }}", json::quote(cast), json::quote(side)))
      .collect();
    println!("{{\n  \"annotated\": [\n{}\n  ]\n}}", items.join(",\n"));
  }
  if sidecars.len() == paths.len() {
    0
  } else {
    1
  }
}

/// Report an `annotate-cast` failure in the active mode: a single-key `{"Error": …}` JSON
/// object on stdout when machine-facing, a plain note on stderr for a human. Returns `code`.
fn annotate_error(as_json: bool, code: i32, message: &str) -> i32 {
  if as_json {
    println!("{{ \"Error\": {{ \"message\": {} }} }}", json::quote(message));
  } else {
    eprintln!("annotate-cast: {message}");
  }
  code
}

/// `export-cast <cast>… [-o <file>]`: render each recording (plus its `.chapters.json`
/// sidecar, when present) into one self-contained offline HTML player page via `beecast-page`.
/// Each cast exports to `<stem>.html` next to it; `-o` overrides the path for a single cast,
/// and `-o -` streams the page to stdout. Human mode prints one ✓/✗ line per cast; with
/// `--json` (or when stdout is not a TTY) a per-cast machine document is emitted instead —
/// except under `-o -`, where stdout IS the page, so the report is the stderr note only.
/// A cast that is not an asciicast fails (exit 1) but the other casts still export.
fn export_casts_cmd(paths: &[String], output: Option<&str>, json_flag: bool) -> i32 {
  use std::io::IsTerminal;
  let streaming = output == Some("-");
  let as_json = !streaming && (json_flag || !std::io::stdout().is_terminal());
  if paths.is_empty() {
    return export_error(
      as_json,
      2,
      "give one or more .cast files, e.g. scsh export-cast ~/.scsh/sessions/<session>/casts/foo.cast",
    );
  }
  if output.is_some() && paths.len() != 1 {
    return export_error(as_json, 2, &format!("-o applies to exactly one cast ({} given)", paths.len()));
  }
  let mut entries: Vec<String> = Vec::new(); // per-cast JSON: {input, output, bytes, chapters} or an error
  let mut worst = 0;
  for path in paths {
    let cast_path = std::path::Path::new(path);
    match export_one_cast(cast_path, output) {
      Ok((out_name, bytes, chapters, warning)) => {
        if !as_json && !streaming {
          ok(&format!("{path} → {out_name} ({bytes} bytes, {chapters} chapters)"));
        } else if streaming && !json_flag {
          eprintln!("✓ {path} → stdout ({bytes} bytes, {chapters} chapters)");
        }
        let warn_field = warning.map(|w| format!(", \"warning\": {}", json::quote(&w))).unwrap_or_default();
        entries.push(format!(
          "    {{ \"input\": {}, \"output\": {}, \"bytes\": {bytes}, \"chapters\": {chapters}{warn_field} }}",
          json::quote(path),
          json::quote(&out_name),
        ));
      }
      Err((problem, fix)) => {
        fail(&format!("{path}: {problem}"));
        hint(&fix);
        entries.push(format!("    {{ \"input\": {}, \"error\": {} }}", json::quote(path), json::quote(&problem)));
        worst = worst.max(1);
      }
    }
  }
  if as_json {
    println!("{{\n  \"exported\": [\n{}\n  ]\n}}", entries.join(",\n"));
  }
  worst
}

/// One successful export: `(output name, page bytes, chapter count, sidecar warning)`.
type ExportSummary = (String, usize, usize, Option<String>);

/// Export one cast: read it, pick up its sidecar (a malformed one warns on stderr and is
/// reported in the JSON entry — both channels — but never fails the export), render, and
/// write (or stream, for `-o -`). Returns an [`ExportSummary`] on success, or an actionable
/// `(✗ problem, → fix)` pair on failure.
fn export_one_cast(cast_path: &Path, output: Option<&str>) -> Result<ExportSummary, (String, String)> {
  let ndjson = std::fs::read_to_string(cast_path).map_err(|e| {
    (format!("cannot read the cast: {e}"), format!("check the path — is {} a recording file?", cast_path.display()))
  })?;
  let (annotation, warning) = match export::load_sidecar(cast_path) {
    export::Sidecar::Found(a) => (Some(a), None),
    export::Sidecar::Absent => (None, None),
    export::Sidecar::Malformed(p) => {
      let w = format!("malformed sidecar {} — exporting without summary/chapters", p.display());
      warn(&w);
      (None, Some(w))
    }
  };
  let stem = export::cast_stem(cast_path);
  let page = export::render_page(&ndjson, &stem, annotation.as_ref()).map_err(|e| {
    (
      e.to_string(),
      "give an asciinema recording (asciicast v1/v2/v3), e.g. one under ~/.scsh/sessions/<session>/casts/".to_string(),
    )
  })?;
  let chapters = annotation.as_ref().map(|a| a.chapters.len()).unwrap_or(0);
  let out_name = if output == Some("-") {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    stdout.write_all(page.as_bytes()).and_then(|()| stdout.flush()).map_err(|e| {
      (format!("cannot stream the page: {e}"), "check what stdout is connected to and retry".to_string())
    })?;
    "stdout".to_string()
  } else {
    let out_path = output.map(std::path::PathBuf::from).unwrap_or_else(|| export::default_output_path(cast_path));
    atomic_write(&out_path, page.as_bytes()).map_err(|e| {
      (
        format!("cannot write {}: {e}", out_path.display()),
        "pick a writable output path with -o <file> (or fix the directory permissions)".to_string(),
      )
    })?;
    out_path.display().to_string()
  };
  Ok((out_name, page.len(), chapters, warning))
}

/// Report an `export-cast` failure in the active mode: a single-key `{"Error": …}` JSON
/// object on stdout when machine-facing, a plain note on stderr for a human. Returns `code`.
fn export_error(as_json: bool, code: i32, message: &str) -> i32 {
  if as_json {
    println!("{{ \"Error\": {{ \"message\": {} }} }}", json::quote(message));
  } else {
    eprintln!("export-cast: {message}");
  }
  code
}

/// After a run, annotate the recordings it produced (best-effort, in parallel), so chapters
/// and summaries appear in the session browser. Also copy new chapters onto matching
/// `.sccache` cast copies so a later cache hit replays chapters with the recording.
/// No-op when Codex is unavailable.
///
/// When `client` is `Some`, each pending cast becomes a live [`daemon::ProcKind::Annotate`]
/// row (indices from `next_proc_index`) so the job page shows annotate progress before the
/// session deregisters. When `client` is `None`, annotation still runs silently.
fn annotate_run_casts(
  root: &Path, cast_paths: Vec<std::path::PathBuf>, client: Option<&daemon::Client>, next_proc_index: &mut usize,
) {
  // Attach chapters that already exist (e.g. a prior annotate) into the result cache first.
  for cast in &cast_paths {
    cache_attach_chapters(root, cast);
  }
  // A cast needs annotation when it has no sidecar yet OR when it was re-recorded after
  // its sidecar was written — "sidecar exists" alone would keep a stale annotation forever.
  let pending: Vec<std::path::PathBuf> = cast_paths
    .into_iter()
    .filter(|c| {
      daemon::chapters_sidecar_path(&c.to_string_lossy())
        .map(|s| {
          annotate::sidecar_is_stale(c, &s)
            && !annotation_in_progress(c)
            && !annotate::automatic_annotation_suppressed(c)
        })
        .unwrap_or(false)
    })
    .collect();
  if pending.is_empty() || !annotate::host_can_annotate() {
    return;
  }
  eprintln!("scsh: annotating {} cast(s) with codex · {} …", pending.len(), annotate_model());
  let model = annotate_model();
  let mut done = 0;
  if let Some(client) = client {
    std::thread::scope(|scope| {
      let mut handles = Vec::with_capacity(pending.len());
      for cast in pending {
        let idx = *next_proc_index;
        *next_proc_index += 1;
        let stem = cast.file_stem().and_then(|s| s.to_str()).unwrap_or("cast");
        let mut label = format!("annotate · {stem}");
        if label.len() > 60 {
          label.truncate(57);
          label.push('…');
        }
        // The annotated cast path rides along so the daemon can point this recording's
        // "chapters: summarizing…" note at the job carrying these annotate rows.
        client.proc_add(
          idx,
          &label,
          daemon::ProcKind::Annotate,
          None,
          Some("codex"),
          Some(model.as_str()),
          None,
          None,
          Some(cast.to_string_lossy().as_ref()),
          None,
        );
        client.proc_start(idx);
        client.proc_note(idx, "summarizing…");
        let model = model.clone();
        let session_id = client.session_id().to_string();
        handles.push(scope.spawn(move || {
          let started = std::time::Instant::now();
          let stem = cast.file_stem().and_then(|s| s.to_str()).unwrap_or("cast");
          // Only pre-register a recording the host can actually produce (tmux + asciinema);
          // a cast path that never materializes leaves the job page an empty player.
          let record_cast = annotate::can_record_annotate().then(|| {
            let casts_dir = runtime::session_casts_dir(&session_id);
            let _ = std::fs::create_dir_all(&casts_dir);
            casts_dir.join(format!(
              "annotate-{}-{}-utc-{}.cast",
              stem.replace('/', "_"),
              runtime::format_utc_timestamp(now_secs()),
              runtime::random_nonce_6()
            ))
          });
          if let Some(ref rc) = record_cast {
            client.proc_cast(idx, &rc.to_string_lossy());
          }
          let result = annotate::annotate_cast(&cast, &model, record_cast.as_deref());
          let elapsed = started.elapsed().as_secs_f64();
          match &result {
            Ok(res) => {
              if let Some(ref cpath) = res.cast_path {
                client.proc_cast(idx, &cpath.to_string_lossy());
              } else {
                // A headless fallback may succeed after the recorded TUI failed. Do not
                // present that failed recording as evidence of the successful annotation.
                client.proc_cast(idx, "");
              }
              client.proc_finish(idx, daemon::ProcStatus::Ok, None, None, elapsed);
            }
            // The distinct failure reason (unreadable cast / model timeout / …) is the
            // Fail row's message, so the job page says more than "no annotation produced".
            Err(err) => client.proc_finish(
              idx,
              daemon::ProcStatus::Fail,
              Some(err.failure_reason()),
              Some(&err.to_string()),
              elapsed,
            ),
          }
          (cast, result.is_ok())
        }));
      }
      for h in handles {
        if let Ok((cast, true)) = h.join() {
          done += 1;
          cache_attach_chapters(root, &cast);
        }
      }
    });
  } else {
    let handles: Vec<_> = pending
      .into_iter()
      .map(|cast| {
        let model = model.clone();
        std::thread::spawn(move || {
          let result = annotate::annotate_cast_with(&cast, &model, annotate::run_codex);
          // No daemon row to carry the reason here, so a failure is at least named on
          // stderr instead of silently folding into the final "annotated N" count.
          if let Err(err) = &result {
            eprintln!("scsh: annotate failed for {}: {err}", cast.display());
          }
          (cast, result.is_ok())
        })
      })
      .collect();
    for h in handles {
      if let Ok((cast, true)) = h.join() {
        done += 1;
        cache_attach_chapters(root, &cast);
      }
    }
  }
  eprintln!("scsh: annotated {done} cast(s)");
}

fn annotation_marker(cast: &Path) -> PathBuf {
  let mut marker = cast.as_os_str().to_os_string();
  marker.push(".annotating");
  PathBuf::from(marker)
}

/// How long an `.annotating` marker is trusted as evidence of live work. Comfortably above
/// the worst honest annotation (transcript render + two 180s model attempts + recorded-TUI
/// teardown); past it the marker is a leftover of a killed annotation, and honoring it
/// would block the cast's re-annotation forever.
const ANNOTATION_MARKER_TTL: Duration = Duration::from_secs(15 * 60);

/// True while a FRESH marker says another process is annotating this cast right now. A
/// stale marker — its process killed before the cleanup trap ran — is deleted on sight, so
/// one interrupted annotation can never permanently silence a recording.
fn annotation_in_progress(cast: &Path) -> bool {
  let marker = annotation_marker(cast);
  let Ok(meta) = std::fs::metadata(&marker) else {
    return false;
  };
  let fresh = meta.modified().ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age < ANNOTATION_MARKER_TTL);
  if !fresh {
    let _ = std::fs::remove_file(&marker);
  }
  fresh
}

/// Start annotation as soon as one run's durable recording exists. The marker prevents
/// the end-of-job catch-up sweep from duplicating live work; the child shell removes it on
/// every exit, while a successful child leaves the chapters sidecar that makes catch-up a no-op.
///
/// The worker is fully detached (double-fork + `setsid`, the daemon's own pattern): an
/// annotation doing its job must survive the launching terminal or agent harness tearing
/// down its process group the moment the run's foreground command returns — exactly what
/// used to kill annotations seconds in while their runs had already succeeded.
fn spawn_cast_annotation(cast: &Path) {
  if !annotate::host_can_annotate() || annotate::automatic_annotation_suppressed(cast) {
    return;
  }
  let Some(sidecar) = daemon::chapters_sidecar_path(&cast.to_string_lossy()) else {
    return;
  };
  if !annotate::sidecar_is_stale(cast, &sidecar) {
    return;
  }
  if annotation_in_progress(cast) {
    return;
  }
  let marker = annotation_marker(cast);
  if std::fs::OpenOptions::new().write(true).create_new(true).open(&marker).is_err() {
    return;
  }
  let Ok(exe) = std::env::current_exe() else {
    let _ = std::fs::remove_file(marker);
    return;
  };
  let script = format!(
    "trap 'rm -f {marker}' EXIT; SCSH_AUTO_ANNOTATE=1 {exe} annotate-cast {cast} --json >/dev/null 2>&1",
    marker = runtime::shell_quote(&marker.to_string_lossy()),
    exe = runtime::shell_quote(&exe.to_string_lossy()),
    cast = runtime::shell_quote(&cast.to_string_lossy()),
  );
  let mut cmd = Command::new("sh");
  cmd.args(["-c", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt;
    // SAFETY: the pre_exec hook uses only async-signal-safe syscalls (fork/setsid/_exit).
    unsafe {
      cmd.pre_exec(daemon::daemon_detach_child);
    }
  }
  if cmd.spawn().is_err() {
    let _ = std::fs::remove_file(marker);
  }
}

/// If `path` contains `/sessions/<id>/`, return that session id (parent of a standalone annotate).
fn parent_session_from_cast_path(path: &str) -> Option<String> {
  let mut comps = std::path::Path::new(path).components();
  while let Some(c) = comps.next() {
    if c.as_os_str() == "sessions" {
      return comps.next().map(|c| c.as_os_str().to_string_lossy().into_owned()).filter(|s| !s.is_empty());
    }
  }
  None
}

#[derive(Clone)]
enum Mode {
  Help(HelpTopic),
  Version,
  InitDemo,
  InstallSkills,
  UpdateSkills,
  List,
  CheckProfile,
  Probe,
  Run,
  UiDemo {
    frames: bool,
  },
  Daemon {
    action: DaemonAction,
  },
  /// Hidden: the long-lived HTTP server process.
  DaemonServe {
    mode: daemon::DaemonMode,
    port: u16,
  },
  /// Hidden: record a command under a PTY as an asciicast (scsh is its own recorder —
  /// this is how image builds become casts with no host asciinema).
  RecordPty {
    cast: PathBuf,
    cols: u16,
    rows: u16,
    argv: Vec<String>,
  },
  /// Browse the failure log (`scsh failures`), with filters and `--stats`.
  Failures,
  /// Browse durable run statistics (`scsh stats`): durations and workload per route.
  Stats,
  /// Show the run-dir prune queue; `--now` forces a janitor pass.
  Prune,
  /// Reclaim old `$SCSH_HOME/sessions/` dirs (dry-run by default; `--apply` to delete).
  Gc,
  /// Summarize + chapter cast recordings with Codex (`annotate-cast <cast>…`).
  AnnotateCasts,
  /// Render cast recordings into self-contained offline HTML player pages
  /// (`export-cast <cast>… [-o <file>]`).
  ExportCasts,
  /// Build the base and/or harness images outside a run (`build-images [harness…]`), streaming
  /// into the session browser — the daemon's images panel spawns this command.
  BuildImages,
  /// Print an embedded agent-followable walkthrough to stdout (`scsh demo [name]`), so a
  /// driving agent needs no path to any checkout — `scsh` on PATH is enough. No name lists them.
  Demo {
    name: Option<String>,
  },
}

#[derive(Clone, Copy)]
enum DaemonAction {
  Start,
  Stop,
  Restart,
  Status,
}

/// Which help page to print. The default (`scsh help` / a bare `scsh`) is a compact
/// overview of the commands; the deep-dive topics keep their detail OUT of the default
/// output (`scsh help run`, `scsh help .scsh.yml`, `scsh help internals`, `scsh help cache`).
#[derive(Clone)]
enum HelpTopic {
  Overview,
  Run,
  Config,
  Internals,
  Cache,
  /// The agent-first contract (`scsh help agent`) — how another agent or harness drives
  /// scsh end to end: discover, gate, run, collect — exit codes and JSON only.
  Agent,
  /// Harness definitions (`scsh help def`) — the `.harness/<name>.yml` format: flat tasks,
  /// workflow steps, gates, and the repeat / do-while loops with loop-carried inputs.
  Defs,
  /// The documented exit-code table (`scsh help exitcodes`).
  ExitCodes,
  /// Focused help for one command (`scsh help <command>`); the string is the canonical name.
  Command(String),
}

/// Canonical command names that `help <command>` documents (also the resolver's vocabulary).
const COMMAND_NAMES: &[&str] = &[
  "run",
  "list",
  "build-images",
  "check-profile",
  "probe",
  "init-demo-project",
  "demo",
  "installskills",
  "updateskills",
  "daemon",
  "failures",
  "stats",
  "prune",
  "gc",
  "annotate-cast",
  "export-cast",
  "version",
];

/// Resolve a `help <token>` argument to a command name, accepting the same non-canonical
/// spellings the command parser does (e.g. `ls` → `list`), so `help ls` works.
fn help_command_alias(token: &str) -> Option<&'static str> {
  let canonical = match token {
    "run" => "run",
    "list" | "ls" => "list",
    "build-images" | "build-image" | "buildimages" => "build-images",
    "check-profile" | "checkprofile" => "check-profile",
    "probe" => "probe",
    "init-demo-project" | "init" | "init-demo" => "init-demo-project",
    "demo" | "demos" => "demo",
    "installskills" | "install-skills" => "installskills",
    "updateskills" | "update-skills" => "updateskills",
    "daemon" => "daemon",
    "failures" => "failures",
    "stats" => "stats",
    "prune" => "prune",
    "gc" => "gc",
    "annotate-cast" | "annotate-casts" | "annotate" => "annotate-cast",
    "export-cast" | "export-casts" | "export" => "export-cast",
    "version" => "version",
    _ => return None,
  };
  Some(canonical)
}

fn version_id() -> String {
  version::display()
}

enum Action {
  List,
  Run,
}

/// A parsed command line: one command, plus the profiles that select which skills run (bare
/// positional names and/or `--profile` for `run`; the one profile name for `check-profile`),
/// the source repos (git URLs/paths for `installskills` / `updateskills` — one or more,
/// installed in order), and the `list` output flags (`--verbose`, `--json`).
struct Cli {
  mode: Mode,
  profile: Option<String>,
  sources: Vec<String>,
  /// Cast files for `annotate-cast` (positional).
  annotate_paths: Vec<String>,
  /// Cast files for `export-cast` (positional).
  export_paths: Vec<String>,
  /// `export-cast -o <file>`: the output path for the (single) cast; `-` streams to stdout.
  output: Option<String>,
  verbose: bool,
  json: bool,
  failures: FailuresOpts,
  prune_now: bool,
  /// Options for `scsh gc` (dry-run by default).
  gc: gc::GcOpts,
  /// Harness names for `build-images` (positional; empty = every harness).
  build_harnesses: Vec<String>,
  /// `build-images --force`: rebuild the selected harness images even when up to date.
  build_force: bool,
  /// `build-images --rebuild-base`: force-rebuild the shared base (`--no-cache`) first.
  build_rebuild_base: bool,
  /// `run --def <name>`: run the named harness definition (a `.harness/<name>.yml` or a
  /// built-in) instead of the repo's `.scsh.yml` skills. Its params come from the environment.
  def: Option<String>,
  /// `run --def <name> --resume-from <session>`: restore every step whose validated result
  /// survives under the named prior session (`$SCSH_HOME/sessions/<id>/results/`) and run
  /// only the steps that never completed — the restart path for a failed workflow job.
  resume_from: Option<String>,
  /// `run --retries N`: this job's daemon-restart budget (default 25; 0 = never
  /// restarted). Propagated as `SCSH_RETRIES` so the session registers it.
  retries: Option<u32>,
  /// `run`/`list`/`check-profile --override-dot-scsh-yml <path>`: use this `.scsh.yml` (and its
  /// sibling `.skills/`) instead of the repo's, so a global skill can drive a fleet without
  /// installing into the target tree. The bundle's skills are installed GLOBALLY inside each
  /// container — claude and cursor discover them natively in their user-level skills dirs
  /// (invocable as `/<name>`); the rest get `tmp/.scsh-skills/` referenced by path. The
  /// target repo's checkout never contains the skill.
  override_dot_scsh_yml: Option<PathBuf>,
  /// `installskills`/`updateskills --global`: install machine-wide under `$SCSH_HOME`
  /// (default `~/.scsh`) instead of into the current repo — no git repo required. See
  /// [`install_skills_global`].
  global: bool,
  /// `run --base <ref>`: point the run clone's mainline branch at `<ref>`, so a skill's
  /// in-container `origin/main..HEAD` is exactly base-vs-HEAD. The caller's own repository
  /// is never touched. See [`resolve_run_base`].
  base: Option<String>,
}

/// Filters and output flags shared by `scsh failures` and `scsh stats`.
#[derive(Default)]
struct FailuresOpts {
  session: Option<String>,
  skill: Option<String>,
  reason: Option<String>,
  stats: bool,
  /// How many trailing events/rows to show (`--last N`; `--last 0` = all; default 50).
  last: Option<usize>,
  /// `scsh stats` route filters.
  harness: Option<String>,
  model: Option<String>,
  /// `scsh stats --raw`: print individual rows instead of aggregates.
  raw: bool,
}

/// Parse cargo-style subcommands. The default (no command) is `help`, so a bare
/// `scsh` is safe and self-explanatory; `run` is the explicit "do it" command.
/// The old `--init-demo-project` / `--help` / `--version`
/// flags keep working as aliases.
fn parse_cli(args: &[String]) -> Result<Cli, String> {
  let mut mode: Option<Mode> = None;
  let mut profiles: Vec<String> = Vec::new();
  let mut sources: Vec<String> = Vec::new();
  let mut annotate_paths: Vec<String> = Vec::new();
  let mut export_paths: Vec<String> = Vec::new();
  let mut output: Option<String> = None;
  let mut verbose = false;
  let mut json = false;
  let mut frames = false;
  let mut failures = FailuresOpts::default();
  let mut prune_now = false;
  let mut gc = gc::GcOpts::default();
  let mut saw_gc_dry_run = false;
  let mut saw_gc_apply = false;
  let mut saw_gc_flag = false;
  let mut build_harnesses: Vec<String> = Vec::new();
  let mut build_force = false;
  let mut build_rebuild_base = false;
  let mut def: Option<String> = None;
  let mut resume_from: Option<String> = None;
  let mut retries: Option<u32> = None;
  let mut override_dot_scsh_yml: Option<PathBuf> = None;
  let mut global = false;
  let mut base: Option<String> = None;
  let mut i = 0;
  while i < args.len() {
    let m = match args[i].as_str() {
      "help" | "-h" | "--help" => {
        // An optional next token selects a deep-dive topic; otherwise the overview.
        let topic = match args.get(i + 1).map(|s| s.as_str()) {
          Some("run") => {
            i += 1;
            HelpTopic::Run
          }
          Some(".scsh.yml") | Some("scsh.yml") | Some(".scsh.yaml") | Some("scsh.yaml") | Some("config")
          | Some("yaml") | Some("yml") | Some("schema") => {
            i += 1;
            HelpTopic::Config
          }
          Some("internals") | Some("internal") => {
            i += 1;
            HelpTopic::Internals
          }
          Some("cache") | Some("caching") => {
            i += 1;
            HelpTopic::Cache
          }
          Some("agent") | Some("agents") | Some("agent-first") => {
            i += 1;
            HelpTopic::Agent
          }
          Some("def") | Some("defs") | Some("definition") | Some("definitions") | Some("harness")
          | Some(".harness") | Some("workflow") | Some("workflows") => {
            i += 1;
            HelpTopic::Defs
          }
          Some("exitcodes") | Some("exit-codes") | Some("exit") => {
            i += 1;
            HelpTopic::ExitCodes
          }
          // Any command name (canonical or a known alias) selects that command's help.
          Some(other) if help_command_alias(other).is_some() => {
            i += 1;
            HelpTopic::Command(help_command_alias(other).unwrap().to_string())
          }
          // A non-flag token we don't recognize is a mistyped topic — say so helpfully.
          Some(other) if !other.starts_with('-') => {
            return Err(format!(
              "unknown help topic '{other}' (commands: {}; topics: agent, .scsh.yml, def, internals, cache, exitcodes)",
              COMMAND_NAMES.join(", ")
            ));
          }
          _ => HelpTopic::Overview,
        };
        Some(Mode::Help(topic))
      }
      "version" | "-V" | "--version" => Some(Mode::Version),
      "run" => Some(Mode::Run),
      "list" | "ls" => Some(Mode::List),
      // `check-profile <name>`: a runtime-free existence check for scripts — the next token is
      // the profile name to test (exit 0 iff it exists with >=1 skill).
      "check-profile" => {
        i += 1;
        let name = args.get(i).ok_or("check-profile needs a profile name, e.g. scsh check-profile multiply")?;
        if name.trim().is_empty() {
          return Err("check-profile name must not be empty".into());
        }
        profiles.push(name.clone());
        Some(Mode::CheckProfile)
      }
      // `probe [profile…]`: report which harness·model routes are runnable on this host —
      // no image build, no container. Exit 0 when at least one probed route is available.
      "probe" => Some(Mode::Probe),
      // Hidden dev command: demo the live board with no container/model (see `ui::demo`).
      "__ui-demo" => Some(Mode::UiDemo { frames: false }),
      "--frames" => {
        frames = true;
        None
      }
      "init-demo-project" | "init" | "--init-demo-project" => Some(Mode::InitDemo),
      // `demo [name]`: print an embedded, agent-followable walkthrough verbatim to stdout —
      // the markdown IS the interface, no checkout path needed. No name lists the demos.
      "demo" | "demos" => {
        let name = match args.get(i + 1).map(|s| s.as_str()) {
          Some(n) if !n.starts_with('-') => {
            i += 1;
            Some(n.to_string())
          }
          _ => None,
        };
        Some(Mode::Demo { name })
      }
      // `installskills [<git-url>…]` / `updateskills [<git-url>…]`: positional source repos
      // (one or more) install skills from those repos, in order, instead of scsh's bundled one.
      "installskills" => Some(Mode::InstallSkills),
      "updateskills" => Some(Mode::UpdateSkills),
      // `failures [--session S] [--skill NAME] [--reason CODE] [--last N] [--stats]`:
      // browse the append-only failure log (see `scsh run`'s "failure log:" hint).
      "failures" => Some(Mode::Failures),
      // `stats [--skill NAME] [--profile P] [--harness H] [--model M] [--raw] [--last N]`:
      // durations and workload sizes per skill and harness·model route (~/.scsh/stats.jsonl).
      "stats" => Some(Mode::Stats),
      "--session" | "--skill" | "--reason" | "--harness" | "--model" => {
        let flag = args[i].clone();
        i += 1;
        let value = args.get(i).ok_or_else(|| format!("{flag} needs a value"))?.clone();
        match flag.as_str() {
          "--session" => failures.session = Some(value),
          "--skill" => failures.skill = Some(value),
          "--harness" => failures.harness = Some(value),
          "--model" => failures.model = Some(value),
          _ => failures.reason = Some(value),
        }
        None
      }
      "--stats" => {
        failures.stats = true;
        None
      }
      "--raw" => {
        failures.raw = true;
        None
      }
      "--last" => {
        i += 1;
        let n = args.get(i).ok_or("--last needs a number (0 = all)")?;
        failures.last = Some(n.parse().map_err(|_| format!("bad --last value '{n}'"))?);
        None
      }
      // `annotate-cast <cast>…`: summarize each recording and detect chapters with
      // Codex on Luna, writing a `<cast>.chapters.json` sidecar.
      "annotate-cast" | "annotate-casts" => Some(Mode::AnnotateCasts),
      // `export-cast <cast>… [-o <file>]`: render each recording (+ its chapters sidecar)
      // into one self-contained offline HTML player page next to it.
      "export-cast" | "export-casts" => Some(Mode::ExportCasts),
      "-o" | "--output" => {
        i += 1;
        let value = args.get(i).ok_or("-o needs a path (or `-` for stdout), e.g. scsh export-cast foo.cast -o -")?;
        output = Some(value.clone());
        None
      }
      // `build-images [harness…] [--force] [--rebuild-base] [--session <id>]`: build the
      // shared base + harness images outside any run (the dashboard's images panel spawns this).
      "build-images" => Some(Mode::BuildImages),
      "--force" => {
        build_force = true;
        None
      }
      "--rebuild-base" => {
        build_rebuild_base = true;
        None
      }
      // `prune [--now]`: show the daemon's run-dir cleanup queue, or force a pass now.
      "prune" => Some(Mode::Prune),
      "--now" => {
        prune_now = true;
        None
      }
      // `gc [--dry-run] | --apply [--days N] [--keep N] [--legacy]`: reclaim old session dirs
      // under $SCSH_HOME/sessions/ (dry-run by default; --apply required to delete).
      "gc" => Some(Mode::Gc),
      "--apply" => {
        saw_gc_flag = true;
        saw_gc_apply = true;
        gc.apply = true;
        None
      }
      "--dry-run" => {
        saw_gc_flag = true;
        saw_gc_dry_run = true;
        gc.apply = false;
        None
      }
      "--days" => {
        saw_gc_flag = true;
        i += 1;
        let n = args.get(i).ok_or("--days needs a number (e.g. --days 30)")?;
        gc.days = n.parse().map_err(|_| format!("bad --days value '{n}'"))?;
        None
      }
      "--keep" => {
        saw_gc_flag = true;
        i += 1;
        let n = args.get(i).ok_or("--keep needs a number (e.g. --keep 50)")?;
        gc.keep = n.parse().map_err(|_| format!("bad --keep value '{n}'"))?;
        None
      }
      "--legacy" => {
        saw_gc_flag = true;
        gc.legacy = true;
        None
      }
      "daemon" => {
        i += 1;
        let sub = args.get(i).ok_or("daemon needs a subcommand: start, stop, restart, or status")?;
        let action = match sub.as_str() {
          "start" => DaemonAction::Start,
          "stop" => DaemonAction::Stop,
          "restart" => DaemonAction::Restart,
          "status" => DaemonAction::Status,
          other => return Err(format!("unknown daemon subcommand '{other}' (try: start, stop, restart, status)")),
        };
        Some(Mode::Daemon { action })
      }
      "__daemon-serve" => {
        let mut mode = daemon::DaemonMode::Ephemeral;
        let mut port = daemon::daemon_port();
        loop {
          i += 1;
          match args.get(i).map(|s| s.as_str()) {
            None => break,
            Some("--mode") => {
              i += 1;
              let m = args.get(i).ok_or("__daemon-serve --mode needs persistent or ephemeral")?;
              mode = daemon::DaemonMode::parse(m).ok_or_else(|| format!("bad daemon mode '{m}'"))?;
            }
            Some("--port") => {
              i += 1;
              let p = args.get(i).ok_or("__daemon-serve --port needs a number")?;
              port = p.parse().map_err(|_| format!("bad port '{p}'"))?;
            }
            Some(other) if other.starts_with('-') => {
              return Err(format!("unknown __daemon-serve option '{other}'"));
            }
            Some(_) => break,
          }
        }
        Some(Mode::DaemonServe { mode, port })
      }
      "__record-pty" => {
        let mut cast: Option<PathBuf> = None;
        let mut cols: u16 = 200;
        let mut rows: u16 = 50;
        let mut argv: Vec<String> = Vec::new();
        loop {
          i += 1;
          match args.get(i).map(|s| s.as_str()) {
            None => break,
            Some("--cast") => {
              i += 1;
              cast = Some(PathBuf::from(args.get(i).ok_or("__record-pty --cast needs a path")?));
            }
            Some("--cols") => {
              i += 1;
              let c = args.get(i).ok_or("__record-pty --cols needs a number")?;
              cols = c.parse().map_err(|_| format!("bad cols '{c}'"))?;
            }
            Some("--rows") => {
              i += 1;
              let r = args.get(i).ok_or("__record-pty --rows needs a number")?;
              rows = r.parse().map_err(|_| format!("bad rows '{r}'"))?;
            }
            Some("--") => {
              argv = args[i + 1..].to_vec();
              i = args.len();
              break;
            }
            Some(other) => return Err(format!("unknown __record-pty option '{other}' (args go after --)")),
          }
        }
        let cast = cast.ok_or("__record-pty needs --cast <path>")?;
        if argv.is_empty() {
          return Err("__record-pty needs a command after --".into());
        }
        Some(Mode::RecordPty { cast, cols, rows, argv })
      }
      "--profile" | "--profiles" => {
        i += 1;
        let name = args.get(i).ok_or("--profile needs a name, e.g. --profile code-review (or default,code-review)")?;
        if name.trim().is_empty() {
          return Err("--profile name must not be empty".into());
        }
        profiles.push(name.clone());
        None
      }
      // `run --def <name>`: run a harness definition instead of the .scsh.yml skills. A bare
      // `scsh --def <name>` is shorthand for `scsh run --def <name>`.
      "--def" => {
        i += 1;
        let name = args.get(i).ok_or("--def needs a harness-definition name, e.g. scsh run --def add")?;
        if name.trim().is_empty() {
          return Err("--def name must not be empty".into());
        }
        def = Some(name.clone());
        if mode.is_none() {
          Some(Mode::Run)
        } else {
          None
        }
      }
      // `run --retries N`: how many times the daemon may restart this job after a
      // terminal failure (resuming completed workflow steps). Every job gets 25 unless
      // told otherwise; 0 opts out of supervision entirely.
      "--retries" => {
        i += 1;
        let n = args.get(i).ok_or("--retries needs a count, e.g. --retries 25 (0 = never restart)")?;
        retries = Some(n.parse().map_err(|_| format!("bad --retries value '{n}'"))?);
        None
      }
      // `run --def <name> --resume-from <session>`: reuse the named prior session's completed
      // step results and run only what never completed.
      "--resume-from" => {
        i += 1;
        let id = args.get(i).ok_or("--resume-from needs a session id, e.g. --resume-from qtsiuf")?;
        if id.trim().is_empty() {
          return Err("--resume-from session id must not be empty".into());
        }
        resume_from = Some(id.trim().to_string());
        None
      }
      // Use an external `.scsh.yml` (and its sibling `.skills/`) instead of the repo's — so a
      // global Cursor skill can drive `scsh run code-review` without polluting the target tree.
      "--override-dot-scsh-yml" => {
        i += 1;
        let path =
          args.get(i).ok_or("--override-dot-scsh-yml needs a path, e.g. --override-dot-scsh-yml ~/.scsh/.scsh.yml")?;
        if path.trim().is_empty() {
          return Err("--override-dot-scsh-yml path must not be empty".into());
        }
        override_dot_scsh_yml = Some(PathBuf::from(path));
        None
      }
      // Diff against an arbitrary base without moving the caller's own `main`:
      // the run clone's mainline is repointed here, the caller's repository is not.
      "--base" => {
        i += 1;
        let spec = args.get(i).ok_or("--base needs a git ref, e.g. --base origin/main")?;
        if spec.trim().is_empty() {
          return Err("--base ref must not be empty".into());
        }
        base = Some(spec.trim().to_string());
        None
      }
      "--verbose" | "-v" => {
        verbose = true;
        None
      }
      "--json" => {
        json = true;
        None
      }
      // Machine-wide install target for `installskills`/`updateskills`.
      "--global" => {
        global = true;
        None
      }
      // After `run` (or `probe`), a bare token is a profile name: `scsh run a b` ==
      // `scsh run --profile a,b`. (A `-`-prefixed token is still an unknown flag, and bare
      // tokens before a command — or after any other command — remain errors.)
      other if matches!(mode, Some(Mode::Run | Mode::Probe)) && !other.starts_with('-') => {
        profiles.push(other.to_string());
        None
      }
      // After `installskills`/`updateskills`, each bare token is a source repo — they're
      // installed in order, as if the command were run once per repo.
      other if matches!(mode, Some(Mode::InstallSkills | Mode::UpdateSkills)) && !other.starts_with('-') => {
        sources.push(other.to_string());
        None
      }
      // After `annotate-cast`, each bare token is a cast file to annotate.
      other if matches!(mode, Some(Mode::AnnotateCasts)) && !other.starts_with('-') => {
        annotate_paths.push(other.to_string());
        None
      }
      // After `export-cast`, each bare token is a cast file to export.
      other if matches!(mode, Some(Mode::ExportCasts)) && !other.starts_with('-') => {
        export_paths.push(other.to_string());
        None
      }
      // After `build-images`, each bare token is a harness name (empty = every harness).
      other if matches!(mode, Some(Mode::BuildImages)) && !other.starts_with('-') => {
        build_harnesses.push(other.to_string());
        None
      }
      other => return Err(format!("unknown command or option '{other}' (try 'scsh help')")),
    };
    if let Some(m) = m {
      if mode.is_some() {
        return Err("only one command may be given at a time".into());
      }
      mode = Some(m);
    }
    i += 1;
  }
  let mode = match mode.unwrap_or(Mode::Help(HelpTopic::Overview)) {
    Mode::UiDemo { .. } => Mode::UiDemo { frames },
    other => other,
  };
  // Positional profiles and any `--profile` values combine into one comma-joined spec
  // (requested_profiles splits on `,`/`;`), so `run a b`, `run --profile a,b`, and
  // `run --profile a b` are all equivalent.
  let profile = if profiles.is_empty() { None } else { Some(profiles.join(",")) };
  // `check-profile` carries its single profile name in the same field; `stats` filters by it.
  if profile.is_some() && !matches!(mode, Mode::Run | Mode::Probe | Mode::CheckProfile | Mode::Stats) {
    return Err(
      "profiles only apply to 'run', 'probe', and 'stats' (e.g. `scsh run code-review` or `scsh stats --profile code-review`)"
        .into(),
    );
  }
  if !sources.is_empty() && !matches!(mode, Mode::InstallSkills | Mode::UpdateSkills) {
    return Err("a skills source (git URL) only applies to 'installskills' or 'updateskills'".into());
  }
  if global && !matches!(mode, Mode::InstallSkills | Mode::UpdateSkills) {
    return Err("--global only applies to 'installskills' or 'updateskills'".into());
  }
  if verbose && !matches!(mode, Mode::List) {
    return Err("--verbose only applies to 'list'".into());
  }
  if json && !matches!(mode, Mode::List | Mode::Probe | Mode::AnnotateCasts | Mode::ExportCasts) {
    return Err("--json only applies to 'list', 'probe', 'annotate-cast', and 'export-cast'".into());
  }
  if (failures.reason.is_some() || failures.stats) && !matches!(mode, Mode::Failures) {
    return Err("--reason/--stats only apply to 'failures'".into());
  }
  if (failures.harness.is_some() || failures.model.is_some() || failures.raw) && !matches!(mode, Mode::Stats) {
    return Err("--harness/--model/--raw only apply to 'stats'".into());
  }
  // `--session` (the session id to report into) is shared by failures/stats and by
  // `build-images`/`run`, which the daemon spawns with a pre-created session id; `--skill`
  // and `--last` are query flags that stay on failures/stats only.
  if failures.session.is_some() && !matches!(mode, Mode::Failures | Mode::Stats | Mode::BuildImages | Mode::Run) {
    return Err("--session only applies to 'failures', 'stats', 'build-images', or 'run'".into());
  }
  if (failures.skill.is_some() || failures.last.is_some()) && !matches!(mode, Mode::Failures | Mode::Stats) {
    return Err("--skill/--last only apply to 'failures' or 'stats'".into());
  }
  if (build_force || build_rebuild_base) && !matches!(mode, Mode::BuildImages) {
    return Err("--force/--rebuild-base only apply to 'build-images'".into());
  }
  if prune_now && !matches!(mode, Mode::Prune) {
    return Err("--now only applies to 'prune' (e.g. `scsh prune --now`)".into());
  }
  if saw_gc_flag && !matches!(mode, Mode::Gc) {
    return Err("--apply/--dry-run/--days/--keep/--legacy only apply to 'gc'".into());
  }
  if saw_gc_apply && saw_gc_dry_run {
    return Err("pass either --apply or --dry-run, not both".into());
  }
  if !annotate_paths.is_empty() && !matches!(mode, Mode::AnnotateCasts) {
    return Err("cast paths only apply to 'annotate-cast'".into());
  }
  if output.is_some() && !matches!(mode, Mode::ExportCasts) {
    return Err("-o only applies to 'export-cast' (e.g. `scsh export-cast foo.cast -o -`)".into());
  }
  if def.is_some() && !matches!(mode, Mode::Run) {
    return Err("--def only applies to 'run' (e.g. `scsh run --def add`)".into());
  }
  if def.is_some() && profile.is_some() {
    return Err("--def selects a harness definition, not a profile — don't combine them".into());
  }
  if resume_from.is_some() && def.is_none() {
    return Err(
      "--resume-from only applies to 'run --def' (e.g. `scsh run --def gorgeous-pipeline --resume-from qtsiuf`)".into(),
    );
  }
  if retries.is_some() && !matches!(mode, Mode::Run) {
    return Err("--retries only applies to 'run' (e.g. `scsh run --def greet --retries 3`)".into());
  }
  if override_dot_scsh_yml.is_some() && !matches!(mode, Mode::Run | Mode::List | Mode::CheckProfile | Mode::Probe) {
    return Err("--override-dot-scsh-yml only applies to 'run', 'list', 'check-profile', and 'probe'".into());
  }
  if override_dot_scsh_yml.is_some() && def.is_some() {
    return Err("--override-dot-scsh-yml and --def are mutually exclusive".into());
  }
  if base.is_some() && !matches!(mode, Mode::Run) {
    return Err("--base only applies to 'run' (e.g. `scsh run code-review --base origin/main`)".into());
  }
  Ok(Cli {
    mode,
    profile,
    sources,
    annotate_paths,
    export_paths,
    output,
    verbose,
    json,
    failures,
    prune_now,
    gc,
    build_harnesses,
    build_force,
    build_rebuild_base,
    def,
    resume_from,
    retries,
    override_dot_scsh_yml,
    global,
    base,
  })
}

/// The profiles requested on the command line, as a set. No `--profile` is the reserved
/// `default` profile (the skills with no `profile:`); a spec may name several, separated by
/// `,` or `;` — e.g. `--profile default,multiply` selects both groups.
fn requested_profiles(spec: Option<&str>) -> std::collections::BTreeSet<String> {
  match spec {
    None => std::iter::once("default".to_string()).collect(),
    Some(s) => s.split([',', ';']).map(str::trim).filter(|p| !p.is_empty()).map(str::to_string).collect(),
  }
}

/// Invocations selected for a run after expanding matrix skills. Those whose profile is in
/// the requested set run; a skill with no `profile:` belongs to the reserved `default` profile.
fn select_invocations(cfg: &config::Config, profile: Option<&str>) -> Vec<ResolvedInvocation> {
  let want = requested_profiles(profile);
  config::expand_invocations(cfg)
    .into_iter()
    .filter(|s| want.contains(s.profile.as_deref().unwrap_or("default")))
    .collect()
}

/// The distinct profile names across expanded invocations, in first-seen order.
fn declared_profiles(cfg: &config::Config) -> Vec<String> {
  let mut out = Vec::new();
  for inv in config::expand_invocations(cfg) {
    let p = inv.profile.as_deref().unwrap_or("default").to_string();
    if !out.contains(&p) {
      out.push(p);
    }
  }
  out
}

// ---------------------------------------------------------------------------
// Preflight + actions
// ---------------------------------------------------------------------------

/// The repo-hygiene half of the preflight, shared by `scsh run` and `scsh run --def`: git
/// installed → inside a git repo → (for a run) a clean working tree and a gitignored `/tmp`.
/// Returns the repo root, or the exit code to return on the first failing check.
fn preflight_git_repo_clean(is_run: bool) -> Result<PathBuf, i32> {
  // 1. git installed.
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return Err(1);
  }

  // 2. inside a git repository.
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return Err(1);
    }
  };

  // For a real run, the repo must be runnable: a clean working tree (the container gets a
  // clone of COMMITTED state) and a gitignored /tmp (build scratch + result stay untracked).
  if is_run {
    let dirty = uncommitted_changes(&root);
    if !dirty.is_empty() {
      fail(
        "working tree has uncommitted changes — scsh runs a clone of committed state, \
so they would not be in the container",
      );
      let shown = dirty.len().min(10);
      for p in &dirty[..shown] {
        hint(&format!("uncommitted: {p}"));
      }
      if dirty.len() > shown {
        hint(&format!("\u{2026}and {} more", dirty.len() - shown));
      }
      hint(&format!(
        "commit or stash them first, then re-run:  {}",
        bold("git add -A && git commit -m \"Committing unstaged changes to run scsh.\"")
      ));
      return Err(1);
    }
    if !tmp_is_gitignored(&root) {
      fail("/tmp is not gitignored in this repository");
      if !root.join(".scsh.yml").is_file() {
        // Fresh repo: don't make them fix this by hand — one command sets it all up.
        hint(&format!("get a ready-to-run project in one command: {}", bold("scsh init-demo-project")));
        hint("(writes .scsh.yml, gitignores /tmp, scaffolds example skills, and commits)");
      } else {
        // Has a config already: scsh still fixes the .gitignore and commits for you.
        hint(&format!("let scsh add /tmp to .gitignore and commit it: {}", bold("scsh init-demo-project")));
      }
      return Err(1);
    }
    // Physical scratch dir for results/logs/cache — gitignored above, always present.
    let _ = std::fs::create_dir_all(root.join("tmp"));
  }
  Ok(root)
}

/// The runtime half of the preflight, shared by `scsh run` and `scsh run --def`: a container
/// runtime is available and (for a run) its engine is up. Returns the runtime or the exit code.
fn preflight_runtime_engine(is_run: bool) -> Result<Runtime, i32> {
  // a container runtime is available.
  let rt = match runtime::detect_runtime() {
    Some(rt) => rt,
    None => {
      let cands = runtime::runtime_candidates(cfg!(target_os = "macos")).join(", ");
      fail(&format!("no container runtime found (looked for: {cands})"));
      hint(install_runtime_hint());
      return Err(1);
    }
  };

  // A snap-packaged Docker can't bind-mount the system temp dir where each clone lives (the
  // container would see an empty home and the skill would crash). Auto-detection already
  // prefers another runtime; warn if it's the only/forced one.
  if rt.name == "docker" && runtime::is_snap_confined(&rt.path) {
    hint("this is snap-packaged Docker, which can't bind-mount the system temp dir;");
    hint("if skills fail to start, use Podman instead (e.g. SCSH_RUNTIME=podman)");
  }

  // For a real run, the runtime's engine must actually be up.
  if is_run && !ui::engine::is_running(&rt.name) {
    fail(&format!("{} is installed but not running", ui::engine::display_name(&rt.name)));
    if let Some(cmd) = ui::engine::start_command(&rt.name, ui::Os::current()) {
      hint(&format!("start it with: {}", bold(&cmd)));
    }
    hint("then re-run 'scsh run'");
    return Err(1);
  }
  Ok(rt)
}

/// `scsh run --def <name>`: run a harness definition (a `.harness/<name>.yml`, a
/// `~/.harness/<name>.yml`, or a built-in) instead of the repo's `.scsh.yml` skills. The
/// same repo-hygiene + runtime preflight applies, but the config steps are replaced by
/// definition discovery and env-parameter validation. The definition's `task` body is
/// materialized into each run clone as `.skills/<name>/SKILL.md`, so the repo stays clean.
fn preflight_then_def(name: &str, session: Option<&str>, resume_from: Option<&str>, base: Option<&str>) -> i32 {
  // git installed → inside a repo → runnable (committed, clean, a gitignored scratch dir). A def
  // run accepts either `tmp/` or `.harness/tmp` as the scratch root, so it does not reuse the
  // `.scsh.yml` preflight (which requires `tmp/` specifically).
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return 1;
  }
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return 1;
    }
  };
  let blockers = def_run_blockers(&root);
  if !blockers.is_empty() {
    fail("this repository is not ready to run a harness definition");
    for b in &blockers {
      hint(&b.message());
      for f in b.fixes() {
        hint(&f);
      }
    }
    return 1;
  }
  // Resolve the base before any image or container work: a bad ref costs one git
  // command here instead of a whole fleet running against the wrong (or an empty) range.
  let base = match base.map(|spec| resolve_run_base(&root, spec)).transpose() {
    Ok(b) => b,
    Err(e) => {
      fail(&e);
      hint(&format!("name a commit this repo already has, e.g. {}", bold("--base origin/main")));
      return 1;
    }
  };
  let scratch = scratch_root(&root).unwrap_or("tmp");

  let discovery = harness_def::discover(&root);
  for w in &discovery.warnings {
    warn(&format!("harness definition: {w}"));
  }
  let def = match discovery.find(name) {
    Some(d) => d.clone(),
    None => {
      fail(&format!("no harness definition named '{name}'"));
      let names: Vec<&str> = discovery.defs.iter().map(|d| d.name.as_str()).collect();
      if names.is_empty() {
        hint("no definitions found — add one under .harness/ or ~/.harness/");
      } else {
        hint(&format!("available: {}", names.join(", ")));
      }
      return 1;
    }
  };

  // Validate the parameters from the environment before touching the runtime: a required
  // param must be set, and every supplied value must match its declared type.
  let mut problems = Vec::new();
  for p in &def.params {
    match std::env::var(&p.name) {
      Ok(v) => {
        if let Err(e) = p.validate_value(&v) {
          problems.push(e);
        }
      }
      Err(_) => {
        if p.required && p.default.is_none() {
          problems.push(format!("param '{}' is required — set it in the environment", p.name));
        }
      }
    }
  }
  if !problems.is_empty() {
    fail(&format!("harness definition '{name}' has {} parameter problem{}", problems.len(), plural(problems.len())));
    for e in &problems {
      hint(e);
    }
    return 1;
  }

  let rt = match preflight_runtime_engine(true) {
    Ok(rt) => rt,
    Err(code) => return code,
  };

  // A workflow definition runs through the DAG orchestrator; a flat one goes through the
  // existing matrix expander below.
  if def.is_workflow() {
    return run_workflow(&rt, &root, &def, session, resume_from, base.as_ref());
  }
  if resume_from.is_some() {
    fail(&format!("'{name}' is a flat definition — --resume-from only applies to workflow definitions (steps:)"));
    hint("flat routes are independent; just run the definition again");
    return 1;
  }

  // `doctor` additionally reports which agent images are built and whose credentials are
  // present, before handing an agent the trivial end-to-end confirm task.
  if name == "doctor" {
    doctor_preflight(&rt);
  }

  // Compile the definition to invocations through the existing matrix expander, then attach
  // the task body so each run clone gets its `.skills/<name>/SKILL.md`.
  let cfg = config::Config { skills: vec![def.to_skill()], terminal: config::Terminal::default() };
  let mut invocations = config::expand_invocations(&cfg);
  for inv in &mut invocations {
    inv.delivery = match &def.task {
      Some(task) => config::SkillDelivery::DirectPrompt(task.clone()),
      None => config::SkillDelivery::Repo,
    };
    // Route the result under the gitignored scratch root (which may be `.harness/tmp`), so the
    // run never writes an untracked file into the working tree.
    inv.result = format!("{scratch}/{}.json", inv.name);
  }

  // Report the ACTUAL gitignored scratch root this run uses (`tmp/` or `.harness/tmp`), not a
  // hardcoded one — the run writes its result, casts, logs, and cache there.
  ok(&format!("git · repo {} · clean · {scratch}/ ignored · def {name}", display_path(&root)));

  // Skip routes whose agent or explicit opencode model is unavailable; fail only when none remain.
  let model_probe = runtime::OpencodeModelProbe::for_selected(&invocations);
  let mut runnable: Vec<&ResolvedInvocation> = Vec::new();
  for inv in &invocations {
    if let Err(msg) = runtime::check_skill_host(inv.harness, inv.model.as_deref(), &model_probe) {
      warn(&format!("skipping '{}' — {msg}", inv.name));
      continue;
    }
    runnable.push(inv);
  }
  if runnable.is_empty() {
    fail("no routes to run — every agent was unavailable on this host");
    hint("check the agent CLIs and credentials (try: scsh run --def doctor)");
    return 1;
  }

  let session_id = session.filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(daemon::new_session_id);
  build_and_run(&rt, &root, &runnable, Some(name), &session_id, "definition", base.as_ref())
}

/// One step's state once it has been decided: either skipped (its `when` was false, or a step it
/// needs was skipped) or run with its validated output fields.
struct StepState {
  skipped: bool,
  outputs: std::collections::HashMap<String, String>,
}

/// The gitignored scratch root a harness-definition run uses for its result/session files:
/// `.harness/tmp` when gitignored (some repos prefer it), else `tmp` when gitignored, else
/// `None` — a def run has no gitignored scratch and must refuse.
fn scratch_root(root: &Path) -> Option<&'static str> {
  if git_status_ok(root, &["check-ignore", "-q", ".harness/tmp"]) {
    Some(".harness/tmp")
  } else if git_status_ok(root, &["check-ignore", "-q", "tmp"]) {
    Some("tmp")
  } else {
    None
  }
}

/// One reason a `scsh run --def` would refuse. Typed rather than prose so callers react to the
/// KIND — the browser reports cleanliness separately from readiness — instead of matching on
/// wording that then cannot be improved without breaking them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DefRunBlocker {
  /// Nothing committed yet: a run clones committed state, so there would be nothing to clone.
  NoCommits,
  /// Uncommitted work, carrying the offending paths — a run would silently not contain it.
  Dirty(Vec<String>),
  /// Neither `tmp/` nor `.harness/tmp` is gitignored, so a run has nowhere to write results.
  NoScratchDir,
}

/// How many offending paths a dirty-tree blocker spells out before summarizing the rest.
const DIRTY_PATHS_SHOWN: usize = 10;

impl DefRunBlocker {
  /// What is wrong, as one line.
  pub(crate) fn message(&self) -> String {
    match self {
      DefRunBlocker::NoCommits => "the repository has no commits yet (scsh runs a clone of committed state)".into(),
      DefRunBlocker::Dirty(paths) => {
        format!("the working tree has {} uncommitted change{}", paths.len(), plural(paths.len()))
      }
      DefRunBlocker::NoScratchDir => {
        "neither tmp/ nor .harness/tmp is gitignored (scsh needs a gitignored scratch dir)".into()
      }
    }
  }

  /// The concrete way out, as the `→` lines that follow the message. A dirty tree names the
  /// files it is actually talking about: "1 uncommitted change" alone sends people hunting,
  /// and — because committing is what unblocks the run — leaves them crediting whatever they
  /// happened to commit with having been a prerequisite.
  pub(crate) fn fixes(&self) -> Vec<String> {
    match self {
      DefRunBlocker::NoCommits => {
        vec![format!("make the first commit: {}", bold("git add -A && git commit -m \"initial\""))]
      }
      DefRunBlocker::Dirty(paths) => {
        let mut out: Vec<String> = paths.iter().take(DIRTY_PATHS_SHOWN).map(|p| format!("uncommitted: {p}")).collect();
        if paths.len() > DIRTY_PATHS_SHOWN {
          out.push(format!("\u{2026}and {} more", paths.len() - DIRTY_PATHS_SHOWN));
        }
        out.push(format!("commit or stash them, then re-run: {}", bold("git add -A && git commit -m \"wip\"")));
        out
      }
      DefRunBlocker::NoScratchDir => vec![
        format!("add a {} line to .gitignore (that is the repo's own tmp/, not the system one)", bold("/tmp")),
        format!("or scaffold a ready-to-run project: {}", bold("scsh init-demo-project")),
      ],
    }
  }
}

/// Reasons a `scsh run --def` would refuse in `root` (empty ⇒ runnable): no commit to clone, a
/// dirty working tree, or no gitignored scratch dir. Shared by the CLI preflight and the daemon
/// so the browser never accepts — or starts — a job the run itself would reject. Assumes `root`
/// is a git repository root.
pub(crate) fn def_run_blockers(root: &Path) -> Vec<DefRunBlocker> {
  let mut out = Vec::new();
  if git_capture(root, &["rev-parse", "--verify", "HEAD"]).is_none() {
    out.push(DefRunBlocker::NoCommits);
  }
  let dirty = uncommitted_changes(root);
  if !dirty.is_empty() {
    out.push(DefRunBlocker::Dirty(dirty));
  }
  if scratch_root(root).is_none() {
    out.push(DefRunBlocker::NoScratchDir);
  }
  out
}

/// Resolve a workflow reference to its current string value: a run parameter (from the
/// environment, else its declared default), or a field of an already-run step's output.
fn resolve_ref(
  reference: &harness_def::Ref, def: &harness_def::HarnessDef, state: &std::collections::HashMap<String, StepState>,
) -> Option<String> {
  match reference {
    harness_def::Ref::Param(n) => {
      std::env::var(n).ok().or_else(|| def.params.iter().find(|p| &p.name == n).and_then(|p| p.default.clone()))
    }
    harness_def::Ref::StepField { step, field } => state.get(step).and_then(|s| s.outputs.get(field).cloned()),
  }
}

/// Resolve one step INPUT: current state first, then the previous do-while iteration's saved
/// outputs — the loop-carried channel validate_step_graph admits, so a body step can consume
/// what the loop's final step produced last round without any committed file. Empty when
/// neither has the value (notably: every loop-carried input on the first iteration).
fn resolve_input(
  reference: &harness_def::Ref, def: &harness_def::HarnessDef, state: &std::collections::HashMap<String, StepState>,
  loop_prev: &std::collections::HashMap<String, StepState>,
) -> String {
  resolve_ref(reference, def, state).or_else(|| resolve_ref(reference, def, loop_prev)).unwrap_or_default()
}

/// Resolve a step's env inputs for this wave. Steps inside a do-while body additionally get
/// `SCSH_LOOP_ITERATION` (1-based), and from iteration 2 on their loop-external step inputs bind
/// to the empty string: data from outside the body is round-0 history, and a later iteration
/// re-reading it would re-litigate findings the loop has already addressed (`params.*` inputs
/// are run constants and stay bound). The loop-carried channel — inputs bound to steps inside
/// the body — stays live through `loop_prev`.
fn step_loop_inputs(
  step: &harness_def::Step, def: &harness_def::HarnessDef, state: &std::collections::HashMap<String, StepState>,
  loop_prev: &std::collections::HashMap<String, StepState>, loop_body: Option<&[String]>, iteration: usize,
) -> Vec<(String, String)> {
  let mut inputs: Vec<(String, String)> = Vec::new();
  for b in &step.inputs {
    let stale = iteration >= 2
      && loop_body.is_some_and(|body| match &b.source {
        harness_def::Ref::StepField { step: src, .. } => !body.iter().any(|inside| inside == src),
        harness_def::Ref::Param(_) => false,
      });
    let value = if stale { String::new() } else { resolve_input(&b.source, def, state, loop_prev) };
    inputs.push((b.name.clone(), value));
  }
  if loop_body.is_some() {
    inputs.push(("SCSH_LOOP_ITERATION".to_string(), iteration.to_string()));
  }
  inputs
}

/// Parse a step's result JSON into its exact top-level fields. Workflow output is a strict
/// machine boundary: duplicate keys are rejected rather than silently choosing one value.
fn parse_result_object(content: &str) -> Result<std::collections::HashMap<String, json::Value>, String> {
  let value = json::parse(content).map_err(|e| format!("result is not valid JSON: {e}"))?;
  let json::Value::Object(obj) = value else {
    return Err("result is not a JSON object".into());
  };
  let mut out = std::collections::HashMap::new();
  for (k, v) in obj {
    if out.insert(k.clone(), v).is_some() {
      return Err(format!("result contains duplicate field '{k}'"));
    }
  }
  Ok(out)
}

/// The workflow-only result contract handed to a skill before it can publish a terminal state.
#[derive(Clone, Copy)]
struct WorkflowResultContract<'a> {
  /// Fields declared by the workflow step.
  outputs: &'a [harness_def::OutputField],
  /// Whether this do-while endpoint also owes the generated repeat-decision field.
  require_do_while_repeat: bool,
}

/// Extract and type-check a workflow step's complete result. Returned strings are the exact
/// values forwarded through environment variables; string arrays remain compact JSON arrays.
fn extract_step_outputs(
  content: &str, contract: WorkflowResultContract<'_>,
) -> Result<std::collections::HashMap<String, String>, String> {
  let obj = parse_result_object(content)?;
  let mut out = std::collections::HashMap::new();
  let expected = contract.outputs.len() + usize::from(contract.require_do_while_repeat);
  if obj.len() != expected {
    let mut extras: Vec<&str> = obj
      .keys()
      .map(String::as_str)
      .filter(|name| {
        !contract.outputs.iter().any(|field| field.name == *name)
          && !(contract.require_do_while_repeat && *name == "SCSH_DO_WHILE_REPEAT")
      })
      .collect();
    extras.sort_unstable();
    if !extras.is_empty() {
      return Err(format!("result contains undeclared field{}: {}", plural(extras.len()), extras.join(", ")));
    }
  }
  for f in contract.outputs {
    let Some(value) = obj.get(&f.name) else {
      return Err(format!("result is missing the '{}' field", f.name));
    };
    let rendered = match (f.ty, value) {
      (harness_def::OutputType::String, json::Value::String(value)) => value.clone(),
      (harness_def::OutputType::Int, json::Value::Number(value))
        if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.0e15 =>
      {
        format!("{}", *value as i64)
      }
      (harness_def::OutputType::Bool, json::Value::Bool(value)) => value.to_string(),
      (harness_def::OutputType::Enum, json::Value::String(value)) if f.choices.iter().any(|choice| choice == value) => {
        value.clone()
      }
      (harness_def::OutputType::StringList, json::Value::Array(values))
        if values.iter().all(|value| matches!(value, json::Value::String(_))) =>
      {
        json::write(value)
      }
      (harness_def::OutputType::Object, json::Value::Object(_)) => json::write(value),
      (harness_def::OutputType::Enum, json::Value::String(value)) => {
        return Err(format!("output '{}' must be one of: {} (got '{value}')", f.name, f.choices.join(", ")));
      }
      (harness_def::OutputType::String, _) => return Err(format!("output '{}' must be a string", f.name)),
      (harness_def::OutputType::Int, _) => return Err(format!("output '{}' must be an integer", f.name)),
      (harness_def::OutputType::Bool, _) => return Err(format!("output '{}' must be true or false", f.name)),
      (harness_def::OutputType::Enum, _) => {
        return Err(format!("output '{}' must be one of: {}", f.name, f.choices.join(", ")));
      }
      (harness_def::OutputType::StringList, _) => {
        return Err(format!("output '{}' must be an array of strings", f.name));
      }
      (harness_def::OutputType::Object, _) => {
        return Err(format!("output '{}' must be a JSON object", f.name));
      }
    };
    out.insert(f.name.clone(), rendered);
  }
  if contract.require_do_while_repeat {
    match obj.get("SCSH_DO_WHILE_REPEAT") {
      Some(json::Value::Bool(value)) => {
        out.insert("SCSH_DO_WHILE_REPEAT".into(), value.to_string());
      }
      Some(_) => return Err("'SCSH_DO_WHILE_REPEAT' must be a boolean".into()),
      None => return Err("result is missing the 'SCSH_DO_WHILE_REPEAT' field".into()),
    }
  }
  Ok(out)
}

/// One-line completion status for a workflow step, built from its declared outputs in
/// contract order: scalar fields render as `name: value`, string values clipped to their
/// first line and 64 characters; list/object fields and empty strings are skipped. `None`
/// when nothing scalar remains — the caller then falls back to the result path.
fn workflow_outputs_glimpse(
  contract: WorkflowResultContract<'_>, outputs: &std::collections::HashMap<String, String>,
) -> Option<String> {
  let parts: Vec<String> = contract
    .outputs
    .iter()
    .filter(|f| !matches!(f.ty, harness_def::OutputType::StringList | harness_def::OutputType::Object))
    .filter_map(|f| {
      let line = first_line(outputs.get(&f.name)?).trim();
      if line.is_empty() {
        return None;
      }
      let clipped = match line.char_indices().nth(63) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line.to_string(),
      };
      Some(format!("{}: {clipped}", f.name))
    })
    .collect();
  if parts.is_empty() {
    None
  } else {
    Some(parts.join(" · "))
  }
}

/// A prior session's persisted result for `run_id`, when it still validates against the step's
/// output contract — the restore criterion for `--resume-from`. A result only ever persists
/// after its run succeeded or failed validation, so "present and valid" is exactly "this step
/// completed"; anything else (missing, unreadable, schema drift) re-runs the step.
fn restored_step_result(
  results_dir: &Path, run_id: &str, contract: WorkflowResultContract<'_>,
) -> Option<(String, std::collections::HashMap<String, String>, PathBuf)> {
  let path = results_dir.join(format!("{}.json", run_id.replace('/', "_")));
  let content = std::fs::read_to_string(&path).ok()?;
  let outputs = extract_step_outputs(&content, contract).ok()?;
  Some((content, outputs, path))
}

/// Build the run invocation for one workflow step: the step's agent, its `SKILL.md` body
/// (prompt + scsh's I/O contract), its resolved inputs as constant env vars, and a per-step
/// result file under the session scratch dir.
fn step_invocation(
  step: &harness_def::Step, run_id: &str, session_dir_rel: &str, inputs: Vec<(String, String)>,
  commit_identity: Option<(String, String)>,
) -> ResolvedInvocation {
  ResolvedInvocation {
    name: run_id.to_string(),
    skill_source: step.id.clone(),
    harness: step.agent.harness,
    model: step.agent.model.clone(),
    effort: step.agent.effort.clone(),
    retry_for: step.retry_for,
    retry_signature_cap: step.retry_signature_cap,
    timeout: None,
    inactivity_timeout: step.inactivity_timeout,
    env: inputs
      .into_iter()
      .map(|(key, value)| config::EnvVar { key, rule: config::EnvRule::Constant(value) })
      .collect(),
    profile: None,
    commits: step.commits,
    commit_identity,
    result: format!("{session_dir_rel}/{run_id}.json"),
    terminal: config::Terminal::default(),
    delivery: config::SkillDelivery::DirectPrompt(step.render_skill_body()),
    artifacts: step.artifacts.iter().map(|a| format!("{session_dir_rel}/{a}")).collect(),
  }
}

/// Clone a workflow invocation for its single result-schema repair attempt. The task runs from
/// the same authoritative source revision; only the prompt gains the concrete validation error
/// and an explicit instruction to satisfy the already-rendered machine contract.
fn schema_repair_invocation(invocation: &ResolvedInvocation, error: &str) -> ResolvedInvocation {
  let mut repaired = invocation.clone();
  if let config::SkillDelivery::DirectPrompt(prompt) = &mut repaired.delivery {
    prompt.push_str(&format!(
      "\n\n## Result correction retry\n\nThe previous attempt completed its work but wrote an invalid result: {error}. Run the task once more from this clean source revision and write a complete result matching the Output contract exactly. This is the only correction retry.\n"
    ));
  }
  repaired
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryDecision {
  Stop,
  /// The identical-failure circuit breaker tripped: stop with its own terminal reason,
  /// so "kept failing the same way" is distinguishable from "not retryable at all".
  StopBreaker,
  Browser,
  Schema,
  Automatic,
}

/// One route's retry verdict for one failed attempt: browser restarts always win, the
/// one schema-correction retry comes next, and everything else is the task retry
/// contract — retryable per [`failure::verdict`], within both its count and wall-clock
/// budgets, and under its consecutive-identical-failure cap.
#[allow(clippy::too_many_arguments)]
fn retry_decision(
  fail_reason: Option<&str>, restart_requested: bool, schema_retry_available: bool, policy: failure::RetryPolicy,
  retries_used: u32, budget_spent_secs: u64, consecutive_identical: u32, retry_enabled: bool, tui: bool,
  first_attempt: bool,
) -> RetryDecision {
  if restart_requested {
    return RetryDecision::Browser;
  }
  if !retry_enabled {
    return RetryDecision::Stop;
  }
  if retries_used >= policy.max_retries {
    return RetryDecision::Stop;
  }
  if fail_reason == Some(failure::reason::RESULT_INVALID) && schema_retry_available {
    return RetryDecision::Schema;
  }
  let Some(reason) = fail_reason else {
    return RetryDecision::Stop;
  };
  if failure::verdict(reason, tui, first_attempt) != failure::Verdict::Retryable {
    return RetryDecision::Stop;
  }
  if consecutive_identical > policy.signature_cap {
    return RetryDecision::StopBreaker;
  }
  if budget_spent_secs >= policy.budget_secs {
    return RetryDecision::Stop;
  }
  RetryDecision::Automatic
}

/// Per-route retry bookkeeping shared by the flat-fleet and workflow loops: when the
/// route first failed (the budget clock), how many consecutive attempts failed with the
/// same signature (the breaker), and how many automatic retries ran (the count ceiling
/// and backoff exponent). The jitter salt is derived from the session and route identity, so a
/// fleet failing together fans back out deterministically.
struct RouteRetryState {
  policy: failure::RetryPolicy,
  salt: u64,
  first_failure_at: Option<Instant>,
  last_signature: Option<String>,
  consecutive_identical: u32,
  retries_used: u32,
}

impl RouteRetryState {
  fn new(retry_for: Option<u64>, signature_cap: Option<u32>, session_id: &str, route: &str) -> RouteRetryState {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    session_id.hash(&mut hasher);
    route.hash(&mut hasher);
    RouteRetryState {
      policy: failure::RetryPolicy::resolve(retry_for, signature_cap),
      salt: hasher.finish(),
      first_failure_at: None,
      last_signature: None,
      consecutive_identical: 0,
      retries_used: 0,
    }
  }

  /// Record one failed attempt: start the budget clock on the first failure and track
  /// the consecutive-identical streak for the breaker.
  fn observe_failure(&mut self, run: &SkillRun) {
    self.first_failure_at.get_or_insert_with(Instant::now);
    let signature =
      failure::failure_signature(run.fail_reason.as_deref().unwrap_or("unknown"), run.fail_detail.as_deref());
    if self.last_signature.as_deref() == Some(signature.as_str()) {
      self.consecutive_identical += 1;
    } else {
      self.last_signature = Some(signature);
      self.consecutive_identical = 1;
    }
  }

  fn budget_spent_secs(&self) -> u64 {
    self.first_failure_at.map(|at| at.elapsed().as_secs()).unwrap_or(0)
  }

  fn budget_left_secs(&self) -> u64 {
    self.policy.budget_secs.saturating_sub(self.budget_spent_secs())
  }

  fn retries_left(&self) -> u32 {
    self.policy.max_retries.saturating_sub(self.retries_used)
  }

  fn record_immediate_retry(&mut self) {
    self.retries_used += 1;
  }

  /// The delay before the next automatic retry, advancing the backoff exponent.
  fn next_backoff_secs(&mut self) -> u64 {
    let delay = self.policy.backoff_delay_secs(self.retries_used, self.salt);
    self.retries_used += 1;
    delay
  }

  /// Rewrite a run that tripped the breaker so the terminal outcome says WHY retries
  /// ended — `retries_exhausted_identical` — rather than repeating the raw last failure.
  fn mark_breaker_tripped(&self, run: &mut SkillRun) {
    let raw = run.fail_reason.clone().unwrap_or_else(|| "unknown".into());
    run.fail_detail = Some(format!(
      "{} consecutive attempts failed identically (last: {raw}); retrying further would burn tokens on a deterministic failure",
      self.consecutive_identical
    ));
    run.fail_reason = Some(failure::reason::RETRIES_EXHAUSTED_IDENTICAL.into());
  }
}

/// Why a retry backoff stopped waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackoffWake {
  /// The delay ran out — take the retry.
  Elapsed,
  /// The browser asked to restart this proc — take the retry immediately.
  Restart,
  /// The job was stopped from the browser — abandon the retry.
  Cancelled,
}

/// Wait out a retry backoff one second at a time, so neither a browser Force restart on the
/// pending attempt nor a stop of the whole job leaves the user staring at a sleeping process.
///
/// Checking for cancellation here is what keeps a stopped job stopped. The daemon ends the
/// session and settles its procs, but it cannot reach into this process; without this the
/// sleeping route would wake up and register a fresh attempt against a session that is
/// already over.
fn backoff_sleep_interruptible(delay_secs: u64, session_id: &str, proc_index: usize) -> BackoffWake {
  for _ in 0..delay_secs {
    if daemon::consume_proc_restart(session_id, proc_index) {
      return BackoffWake::Restart;
    }
    if daemon::session_cancelled(session_id) {
      return BackoffWake::Cancelled;
    }
    std::thread::sleep(Duration::from_secs(1));
  }
  // A zero-length or fully elapsed wait still must not step over a stop.
  if daemon::session_cancelled(session_id) {
    BackoffWake::Cancelled
  } else {
    BackoffWake::Elapsed
  }
}

/// Run one workflow route with the same retry contract as a flat run. Every fresh harness
/// execution gets a fresh proc row linked to its predecessor: automatic retries with
/// exponential backoff for as long as the task's count and wall-clock budgets allow (unless the
/// identical-failure breaker trips first), one schema-correction retry, and unlimited
/// explicit browser restarts.
#[allow(clippy::too_many_arguments)]
fn run_workflow_step_with_retries(
  invocation: &ResolvedInvocation, step: &harness_def::Step, rt: &Runtime, root: &Path, secs: u64,
  initial_proc: ui::screen::Proc, ui: &ui::screen::LiveUi, caller_tip: Option<&str>, base: Option<&RunBase>,
  daemon_client: Option<std::sync::Arc<daemon::Client>>, session_id: &str,
) -> SkillRun {
  let contract = WorkflowResultContract {
    outputs: &step.outputs,
    require_do_while_repeat: step.do_while.is_some()
      && !step.outputs.iter().any(|output| output.name == "SCSH_DO_WHILE_REPEAT"),
  };
  let mut retry = RouteRetryState::new(step.retry_for, step.retry_signature_cap, session_id, &invocation.name);
  let mut proc = initial_proc;
  let mut proc_index = proc.index();
  let mut attempts = 0u64;
  let mut schema_retry_used = false;
  let mut repaired_invocation: Option<ResolvedInvocation> = None;
  loop {
    attempts += 1;
    let attempt_started = Instant::now();
    let current_invocation = repaired_invocation.as_ref().unwrap_or(invocation);
    let mut run = run_one_skill(
      current_invocation,
      rt,
      root,
      secs,
      proc.clone(),
      caller_tip,
      caller_tip,
      base,
      Some(contract),
      daemon_client.clone(),
      session_id,
    );
    run.duration_secs = attempt_started.elapsed().as_secs_f64();
    run.proc_index = proc_index;
    run.attempts = attempts;
    if run.ok {
      daemon::consume_proc_restart(session_id, proc_index);
      return run;
    }

    retry.observe_failure(&run);
    let restart_requested = daemon::consume_proc_restart(session_id, proc_index);
    let invalid_result = run.fail_reason.as_deref() == Some(failure::reason::RESULT_INVALID);
    let decision = retry_decision(
      run.fail_reason.as_deref(),
      restart_requested,
      !schema_retry_used,
      retry.policy,
      retry.retries_used,
      retry.budget_spent_secs(),
      retry.consecutive_identical,
      failure::retry_enabled(),
      invocation.harness.is_tui(),
      attempts == 1,
    );
    if decision == RetryDecision::Stop || decision == RetryDecision::StopBreaker {
      // An invalid-result attempt returns with its proc row still open (run_one_skill
      // leaves it to the orchestrator, which owns the correction retry) — settle it.
      if invalid_result {
        let why = format!(
          "invalid workflow result: {}",
          run.fail_detail.as_deref().unwrap_or("result did not match the declared schema")
        );
        let detail = skill_fail_detail(&why, invocation.harness, run.run_dir.as_deref(), run.log.as_deref());
        proc.finish_fail(failure::reason::RESULT_INVALID, Some(&detail));
      }
      if decision == RetryDecision::StopBreaker {
        retry.mark_breaker_tripped(&mut run);
      }
      return run;
    }

    let reason = match decision {
      RetryDecision::Browser => failure::reason::RESTART_REQUESTED,
      RetryDecision::Schema => failure::reason::RESULT_INVALID,
      RetryDecision::Automatic => run.fail_reason.as_deref().unwrap_or("unknown"),
      RetryDecision::Stop | RetryDecision::StopBreaker => unreachable!("terminal decisions returned above"),
    };
    failure::log_retry(session_id, &invocation.name, invocation.harness.as_str(), invocation.model.as_deref(), reason);
    if decision == RetryDecision::Schema {
      retry.record_immediate_retry();
      schema_retry_used = true;
      let error = run.fail_detail.as_deref().unwrap_or("result did not match the declared schema");
      repaired_invocation = Some(schema_repair_invocation(invocation, error));
    }
    if invalid_result {
      let why = format!(
        "invalid workflow result: {}",
        run.fail_detail.as_deref().unwrap_or("result did not match the declared schema")
      );
      let detail = skill_fail_detail(&why, invocation.harness, run.run_dir.as_deref(), run.log.as_deref());
      proc.finish_fail(failure::reason::RESULT_INVALID, Some(&detail));
      if !keep_run_dirs() {
        if let Some(clone) = &run.clone_dir {
          let _ = std::fs::remove_dir_all(clone);
        }
      }
    }

    let label = format!("{}: {} (retry)", invocation.harness.as_str(), invocation.name);
    let next = ui.proc(label.clone(), false);
    if let Some(client) = &daemon_client {
      client.proc_add(
        next.index(),
        &label,
        daemon::ProcKind::Skill,
        Some(&invocation.name),
        Some(invocation.harness.as_str()),
        invocation.model.as_deref(),
        Some(&step.id),
        None,
        None,
        Some(proc_index),
      );
    }
    proc_index = next.index();
    proc = next;
    // Schema and browser retries fire immediately; automatic retries back off so a fleet
    // does not hammer a provider mid-incident. The wait is visible on the pending row and
    // a browser Force restart on it cuts the wait short.
    if decision == RetryDecision::Automatic {
      let delay = retry.next_backoff_secs();
      proc.note(&format!(
        "retrying in ~{} after {reason} (retry {} of {}, {} and {} retries left)",
        ui::clock::format_elapsed(delay as f64),
        retry.retries_used,
        retry.policy.max_retries,
        ui::clock::format_elapsed(retry.budget_left_secs() as f64),
        retry.retries_left(),
      ));
      match backoff_sleep_interruptible(delay, session_id, proc_index) {
        BackoffWake::Restart => proc.note("retrying now (browser restart)"),
        BackoffWake::Cancelled => {
          proc.note("job stopped — not retrying");
          proc.finish_fail(failure::reason::FORCE_STOPPED, Some("stopped from the session browser"));
          run.fail_reason = Some(failure::reason::FORCE_STOPPED.into());
          return run;
        }
        BackoffWake::Elapsed => {}
      }
    } else if decision == RetryDecision::Schema {
      proc.note(&format!("retrying after {reason} (retry {} of {})", retry.retries_used, retry.policy.max_retries));
    } else {
      proc.note(&format!("retrying after {reason}"));
    }
  }
}

/// Build (or reuse) the base + per-harness images a workflow's steps need, reporting each as a
/// build proc row. Returns the first build failure, if any.
fn ensure_workflow_images(
  rt: &Runtime, ui: &ui::screen::LiveUi, daemon_client: &Option<std::sync::Arc<daemon::Client>>,
  harnesses: &[config::Harness], session_id: &str,
) -> Result<(), (String, i32)> {
  let (uid, gid) = runtime::host_ids();
  let df = runtime::dockerfile_for_runtime(&rt.name);
  if rt.name == "container" && runtime::apple_dockerfile_too_large(&df) {
    return Err((runtime::apple_dockerfile_too_large_message(df.len()), 1));
  }
  let tz = runtime::host_timezone();
  let build_one =
    |label: String, tag: &str, target: &str, fp: &str, harness: Option<config::Harness>| -> Result<(), (String, i32)> {
      // Cast mode: the recorded player is the UI (tail=false — no text-log echo). The
      // proc's "harness" is what is actually running — `podman build`, not the harness
      // whose image is being built (that one is already in the label).
      let builder = format!("{} build", backend_name(&rt.name));
      let p = ui.proc(label.clone(), false);
      if let Some(c) = daemon_client {
        c.proc_add(p.index(), &label, daemon::ProcKind::Build, None, Some(&builder), None, None, None, None, None);
      }
      p.start();
      let stem = harness.map(|h| h.as_str()).unwrap_or("base");
      match run_build(&p, &rt.name, tag, target, &df, uid, gid, fp, false, daemon_client.as_deref(), stem, session_id) {
        Ok(()) => {
          p.finish_ok(None);
          Ok(())
        }
        Err(e) => {
          p.finish_fail(failure::reason::BUILD_FAILED, Some(&e.0));
          Err(e)
        }
      }
    };
  let base_fp = runtime::base_image_fingerprint(&df, uid, gid, &tz);
  if !runtime::image_is_up_to_date(&rt.name, runtime::BASE_IMAGE_TAG, &base_fp) {
    build_one(
      format!("using {} · build base", backend_name(&rt.name)),
      runtime::BASE_IMAGE_TAG,
      runtime::BASE_IMAGE_TARGET,
      &base_fp,
      None,
    )?;
  }
  for &h in harnesses {
    let spec = runtime::image_build_spec(h, &df, uid, gid, &tz);
    if !runtime::image_is_up_to_date(&rt.name, &spec.tag, &spec.fingerprint) {
      build_one(
        format!("using {} · build {}", backend_name(&rt.name), h.as_str()),
        &spec.tag,
        &spec.target,
        &spec.fingerprint,
        Some(h),
      )?;
    }
  }
  Ok(())
}

/// Run a workflow definition: walk its DAG, running each step (via the same one-shot primitive a
/// flat run uses) with its inputs bound from params and upstream outputs, validating each step's
/// typed output, evaluating `when:` gates (a false gate — or a skipped dependency — skips the
/// step), and running independent runnable steps in parallel. One session, one live board.
///
/// With `resume_from`, the walk first consults the named prior session's persisted results
/// (`$SCSH_HOME/sessions/<id>/results/<run_id>.json`): every step whose result is present and
/// still validates against its output contract is restored without a container — loop
/// iterations included, since the run id carries the iteration number — and only the steps
/// that never completed actually run. Commits a restored step made are already on the caller's
/// branch from the prior run, so nothing is re-integrated.
fn run_workflow(
  rt: &Runtime, root: &Path, def: &harness_def::HarnessDef, session: Option<&str>, resume_from: Option<&str>,
  base: Option<&RunBase>,
) -> i32 {
  use std::collections::HashMap;
  ui::signals::install();

  // Resolve the resume source before anything registers: a vanished results dir (pruned old
  // session) degrades to a normal full run with a warning, never a hard failure.
  let resume: Option<(String, PathBuf)> = resume_from.and_then(|old| {
    let dir = runtime::session_results_dir(old);
    if dir.is_dir() {
      Some((old.to_string(), dir))
    } else {
      warn(&format!(
        "nothing to resume from session '{old}' — no results under {} — running every step",
        dir.display()
      ));
      None
    }
  });
  if let Some((old, dir)) = &resume {
    ok(&format!("resuming from session {old} — completed step results under {} are reused", dir.display()));
  }

  // One session for the whole workflow (mirrors build_and_run's daemon attach).
  let session_id = session.filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(daemon::new_session_id);
  // Persist the start recipe (def name + env-resolved params) so the browser can restart or
  // resume this job later even when it was CLI-started — those env params were never the
  // daemon's to see. For daemon-spawned jobs this rewrites the same values it passed in.
  let recipe_params: Vec<(String, String)> =
    def.params.iter().filter_map(|p| std::env::var(&p.name).ok().map(|v| (p.name.clone(), v))).collect();
  // Record the RESOLVED base commit, not the ref the caller typed: a restart days later must
  // review against the same commit even if `origin/main` has moved on since.
  daemon::write_start_recipe(&session_id, Some(&def.name), None, &recipe_params, base.map(|b| b.sha.as_str()));
  let mut daemon_session = DaemonSession { client: None, ping_active: None, registered: false };
  if daemon::ensure_for_run().is_ok() {
    let client = std::sync::Arc::new(daemon::Client::new(session_id.clone()));
    let skill_meta: Vec<(&str, &str)> = def.steps.iter().map(|s| (s.id.as_str(), s.agent.harness.as_str())).collect();
    let workflow = daemon::workflow_meta_from_def(def);
    if client.register_session_with_workflow(
      &repo_path_for_session(root),
      &current_branch(root),
      Some(&def.name),
      "workflow",
      &skill_meta,
      workflow.as_ref(),
      None,
    ) {
      ok(&format!("track progress at {}", client.session_url()));
      let ping_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
      let ping_flag = std::sync::Arc::clone(&ping_active);
      let ping_client = std::sync::Arc::clone(&client);
      std::thread::spawn(move || {
        while ping_flag.load(std::sync::atomic::Ordering::Relaxed) {
          ping_client.ping();
          std::thread::sleep(Duration::from_secs(2));
        }
      });
      daemon_session.client = Some(client);
      daemon_session.ping_active = Some(ping_active);
      daemon_session.registered = true;
    }
  }
  let daemon_client = daemon_session.client.clone();
  let ui = ui::screen::LiveUi::new(console::user_attended_stderr(), daemon_client.clone());

  // Build the images every step agent needs (in first-seen order).
  let mut harnesses = Vec::new();
  for s in &def.steps {
    if !harnesses.contains(&s.agent.harness) {
      harnesses.push(s.agent.harness);
    }
  }
  if let Err((msg, code)) = ensure_workflow_images(rt, &ui, &daemon_client, &harnesses, &session_id) {
    ui.finish();
    fail(&msg);
    return code;
  }

  let secs = now_secs();
  let stamp = runtime::format_utc_timestamp(secs);
  // Tip of the caller's branch before each wave. Commit-enabled steps rebase onto this, then
  // we refresh it so the next wave's clone sees those commits and its own packdiff range is
  // only what *that* step added (same contract as the flat `build_and_run` path).
  let mut caller_tip = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
  // The host repo's own git identity, resolved once — steps declaring `commit-identity: runner`
  // author their commits as the person running the pipeline, not as the notes bot.
  let runner_identity = runner_commit_identity(root);
  let mut runner_identity_warned = false;
  let original_caller_tip = caller_tip.clone();
  let session_dir_rel = format!("{}/scsh/{session_id}", scratch_root(root).unwrap_or("tmp"));
  let mut do_while_end_for: HashMap<String, String> = HashMap::new();
  let mut do_while_bodies: HashMap<String, Vec<String>> = HashMap::new();
  for end in def.steps.iter().filter(|s| s.do_while.is_some()) {
    let body: Vec<String> = harness_def::do_while_body(&def.steps, end).into_iter().map(str::to_string).collect();
    for id in &body {
      do_while_end_for.insert(id.clone(), end.id.clone());
    }
    do_while_bodies.insert(end.id.clone(), body);
  }

  // Declare EVERY step as a proc row up front, in definition order — the board and the
  // session browser show the whole job's shape (step k/n, what it needs) from the first
  // paint. A gated-off step later finishes as ⊘ skipped instead of silently never existing.
  let total_steps = def.steps.len();
  let mut step_procs: HashMap<String, ui::screen::Proc> = HashMap::new();
  for (i, s) in def.steps.iter().enumerate() {
    if s.is_loop() || do_while_end_for.contains_key(&s.id) {
      continue; // loop iterations appear only when they actually start
    }
    let label = format!("{}: {}", s.agent.harness.as_str(), s.id);
    let p = ui.proc(label.clone(), false);
    let mut note = format!("step {}/{total_steps}", i + 1);
    if !s.needs.is_empty() {
      note.push_str(&format!(" · needs {}", s.needs.join(", ")));
    }
    if let Some(c) = &daemon_client {
      c.proc_add(
        p.index(),
        &label,
        daemon::ProcKind::Skill,
        Some(&s.id),
        Some(s.agent.harness.as_str()),
        s.agent.model.as_deref(),
        Some(&s.id),
        None,
        None,
        None,
      );
    }
    p.note(&note);
    step_procs.insert(s.id.clone(), p);
  }
  // Annotate procs (added after the run) continue past the last declared step/build index.
  let mut next_annotate_idx = step_procs.values().map(|p| p.index()).max().map(|m| m + 1).unwrap_or(0);
  ui.pin_board_to_top();

  // Walk the DAG: a step is decidable once every step it needs has a state (run or skipped).
  let mut state: HashMap<String, StepState> = HashMap::new();
  let mut repeat_done: HashMap<String, usize> = HashMap::new();
  // Previous do-while iteration's outputs, keyed by the loop's final step — the loop-carried
  // channel `resolve_input` falls back to, so a body step can read what the deciding step
  // produced LAST round (review feedback, accumulated notes) without committing any file.
  let mut loop_prev: HashMap<String, StepState> = HashMap::new();
  let mut ran_count = 0usize;
  let mut skipped_count = 0usize;
  let mut failure: Option<String> = None;
  while state.len() < def.steps.len() && failure.is_none() {
    let ready: Vec<&harness_def::Step> = def
      .steps
      .iter()
      .filter(|s| !state.contains_key(&s.id) && s.needs.iter().all(|n| state.contains_key(n)))
      .collect();
    if ready.is_empty() {
      break; // acyclic ⇒ this only happens once every step is decided
    }

    // Partition this wave into steps to skip (gate false, or a needed step was skipped) and to run.
    let mut to_run: Vec<&harness_def::Step> = Vec::new();
    for s in ready {
      let skipped_need = s.needs.iter().find(|n| state.get(*n).is_some_and(|st| st.skipped));
      let when_ok = s.when.as_ref().is_none_or(|w| harness_def::when_holds(w, &|r| resolve_ref(r, def, &state)));
      if skipped_need.is_some() || !when_ok {
        let why = match skipped_need {
          Some(n) => format!("skipped — needs '{n}', which was skipped"),
          None => "skipped — its when: gate is false".to_string(),
        };
        if let Some(p) = step_procs.remove(&s.id) {
          p.finish_skipped(&why);
        }
        skipped_count += 1;
        state.insert(s.id.clone(), StepState { skipped: true, outputs: HashMap::new() });
      } else {
        to_run.push(s);
      }
    }
    if to_run.is_empty() {
      continue;
    }
    ran_count += to_run.len();

    // Resolve inputs and pair each runnable step with its pre-declared proc row. A step whose
    // result survives in the resume source is restored on the spot (green row, no container);
    // only the rest of the wave spawns work.
    let mut invs: Vec<ResolvedInvocation> = Vec::new();
    let mut procs: Vec<ui::screen::Proc> = Vec::new();
    let mut live_steps: Vec<&harness_def::Step> = Vec::new();
    let mut run_ids: Vec<String> = Vec::new();
    let mut restored_runs: Vec<(String, SkillRun)> = Vec::new();
    for s in &to_run {
      let loop_key = do_while_end_for.get(&s.id).map(String::as_str).unwrap_or(&s.id);
      let iteration = repeat_done.get(loop_key).copied().unwrap_or(0) + 1;
      let loop_body = do_while_end_for.get(&s.id).and_then(|end| do_while_bodies.get(end)).map(Vec::as_slice);
      let inputs = step_loop_inputs(s, def, &state, &loop_prev, loop_body, iteration);
      let run_id = if let Some(end) = do_while_end_for.get(&s.id) {
        format!("{}-while-{}-{iteration}", s.id, end)
      } else {
        s.iteration_run_id(iteration)
      };
      let p = if s.is_loop() || do_while_end_for.contains_key(&s.id) {
        let label = format!("{}: {} · iteration {iteration}", s.agent.harness.as_str(), s.id);
        let p = ui.proc(label.clone(), false);
        if let Some(c) = &daemon_client {
          c.proc_add(
            p.index(),
            &label,
            daemon::ProcKind::Skill,
            Some(&run_id),
            Some(s.agent.harness.as_str()),
            s.agent.model.as_deref(),
            Some(&s.id),
            None,
            None,
            None,
          );
        }
        p.note(&format!(
          "{} iteration {iteration}",
          if do_while_end_for.contains_key(&s.id) { "do-while" } else { s.loop_kind() }
        ));
        p
      } else {
        step_procs.remove(&s.id).expect("every undecided step has its pre-declared proc")
      };
      if let Some((old_sid, results_dir)) = &resume {
        let contract = WorkflowResultContract {
          outputs: &s.outputs,
          require_do_while_repeat: s.do_while.is_some()
            && !s.outputs.iter().any(|output| output.name == "SCSH_DO_WHILE_REPEAT"),
        };
        if let Some((content, outputs, old_path)) = restored_step_result(results_dir, &run_id, contract) {
          if let Some(dest) = fleet::persist_skill_result(&session_id, &run_id, &old_path) {
            if let Some(c) = &daemon_client {
              c.proc_result(p.index(), &dest);
            }
          }
          p.finish_ok(Some(&format!("restored from session {old_sid} — result reused, no container run")));
          let mut run = SkillRun::cached(None, content, Some(outputs));
          run.proc_index = p.index();
          restored_runs.push((run_id.clone(), run));
          run_ids.push(run_id);
          continue;
        }
      }
      let commit_identity = match s.commit_identity {
        harness_def::CommitIdentity::Runner => {
          if runner_identity.is_none() && !runner_identity_warned {
            warn("this repo has no git user.name/user.email — 'commit-identity: runner' steps fall back to the scsh bot identity");
            runner_identity_warned = true;
          }
          runner_identity.clone()
        }
        harness_def::CommitIdentity::Notes => None,
      };
      invs.push(step_invocation(s, &run_id, &session_dir_rel, inputs, commit_identity));
      procs.push(p);
      live_steps.push(s);
      run_ids.push(run_id);
    }

    // Run the wave in parallel — independent steps proceed at once.
    let mut results: Vec<(String, SkillRun)> = std::thread::scope(|scope| {
      let ui_ref = &ui;
      let handles: Vec<_> = invs
        .iter()
        .zip(procs)
        .zip(live_steps.iter())
        .map(|((inv, p), step)| {
          let dc = daemon_client.clone();
          let caller_tip_ref = caller_tip.as_deref();
          let id = inv.name.clone();
          let sid = session_id.as_str();
          scope.spawn(move || {
            let run =
              run_workflow_step_with_retries(inv, step, rt, root, secs, p, ui_ref, caller_tip_ref, base, dc, sid);
            (id, run)
          })
        })
        .collect();
      handles
        .into_iter()
        .map(|h| {
          h.join()
            .unwrap_or_else(|_| (String::new(), SkillRun::failed(failure::reason::THREAD_PANICKED, None, None, None)))
        })
        .collect()
    });
    results.append(&mut restored_runs);

    // Record each step's outcome in definition order; the first failure (run failure or an
    // output that does not match the declared schema) aborts the workflow. Commit-enabled
    // steps then rebase onto the caller and pack a ⇄ commits diff — before the next wave
    // clones, so later steps see earlier commits (the greet fake-PR chain depends on this).
    let mut by_id: HashMap<String, SkillRun> = results.into_iter().collect();
    for (s, run_id) in to_run.iter().zip(&run_ids) {
      let Some(run) = by_id.remove(run_id) else { continue };
      if !run.ok {
        failure = Some(format!("step '{}' failed ({})", s.id, run.fail_reason.as_deref().unwrap_or("unknown")));
        break;
      }
      match run.workflow_outputs.clone() {
        Some(outputs) => {
          let loop_key = do_while_end_for.get(&s.id).map(String::as_str).unwrap_or(&s.id);
          let completed = repeat_done.get(loop_key).copied().unwrap_or(0) + 1;
          let break_requested = s.break_loop && outputs.get("SCSH_LOOP_BREAK").is_some_and(|value| value == "true");
          let again = if break_requested {
            false
          } else if let Some(total) = s.repeat {
            completed < total
          } else if s.do_while.is_some() {
            let holds = outputs.get("SCSH_DO_WHILE_REPEAT").is_some_and(|value| value == "true");
            // A definition's own `max-iterations` is the budget the AUTHOR chose; scsh's
            // backstop is the ceiling nobody may exceed. Report them differently: the first
            // is a loop that used up its allowance, the second is a loop that ran away.
            let ceiling = s.max_iterations.unwrap_or(harness_def::DO_WHILE_MAX_ITERATIONS);
            if holds && completed >= ceiling {
              failure = Some(match s.max_iterations {
                Some(cap) => format!(
                  "step '{}' reached its max-iterations ({cap}) with its condition still true — the loop did not converge",
                  s.id
                ),
                None => format!(
                  "step '{}' hit the do-while backstop ({} iterations) with its condition still true",
                  s.id,
                  harness_def::DO_WHILE_MAX_ITERATIONS
                ),
              });
              break;
            }
            holds
          } else {
            false
          };
          if break_requested {
            state.insert(s.id.clone(), StepState { skipped: false, outputs });
            if let Some(body) = do_while_bodies.get(loop_key) {
              for id in body.iter().filter(|id| *id != &s.id) {
                if state.contains_key(id) {
                  continue;
                }
                let skipped = def.steps.iter().find(|step| &step.id == id).expect("validated loop step");
                let run_id = format!("{}-while-{}-{completed}", skipped.id, loop_key);
                let label = format!("{}: {} · iteration {completed}", skipped.agent.harness.as_str(), skipped.id);
                let p = ui.proc(label.clone(), false);
                if let Some(c) = &daemon_client {
                  c.proc_add(
                    p.index(),
                    &label,
                    daemon::ProcKind::Skill,
                    Some(&run_id),
                    Some(skipped.agent.harness.as_str()),
                    skipped.agent.model.as_deref(),
                    Some(&skipped.id),
                    None,
                    None,
                    None,
                  );
                }
                p.finish_skipped(&format!("skipped — '{}' broke the loop", s.id));
                skipped_count += 1;
                state.insert(id.clone(), StepState { skipped: true, outputs: HashMap::new() });
              }
            }
          } else if again {
            repeat_done.insert(loop_key.to_string(), completed);
            // The final step's outputs become the NEXT iteration's loop-carried values.
            loop_prev.insert(s.id.clone(), StepState { skipped: false, outputs });
            if let Some(body) = do_while_bodies.get(loop_key) {
              for id in body {
                state.remove(id);
              }
            }
          } else {
            state.insert(s.id.clone(), StepState { skipped: false, outputs });
          }
        }
        None => {
          failure = Some(format!("step '{}' completed without validated workflow outputs", s.id));
          break;
        }
      }
      if s.commits {
        if let (Some(b), Some(clone)) = (caller_tip.as_deref(), run.clone_dir.as_ref()) {
          let inv = invs.iter().find(|i| i.name == *run_id).expect("invocation for step in this wave");
          match integrate_commits(root, clone, b, &s.id, &stamp) {
            Ok(None) => {}
            Ok(Some(Integration::Applied { count, range })) => {
              ok(&format!(
                "{}: brought in {count} commit{} (rebased onto {})",
                s.id,
                plural(count),
                current_branch(root)
              ));
              caller_tip = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
              if let (Some(from), Some(to)) = (original_caller_tip.as_deref(), caller_tip.as_deref()) {
                pack_job_diff(root, &session_id, from, to);
              }
              // The browser reveals its all-commits link when the first step diff is
              // registered, so make the whole-job artifact current before that event.
              pack_step_diff(root, &session_id, inv, &run, range, daemon_client.as_deref());
            }
            Ok(Some(Integration::Saved { branch, count, range })) => {
              if let Some((_, tip)) = &range {
                // A commit-producing workflow step may deliberately rewrite its input
                // history. The caller branch stays untouched for safety, but this saved
                // tip is now the workflow's authoritative revision: every dependent wave
                // must clone it rather than independently guessing which branch to inspect.
                caller_tip = Some(tip.clone());
              }
              if let (Some(from), Some(to)) = (original_caller_tip.as_deref(), caller_tip.as_deref()) {
                pack_job_diff(root, &session_id, from, to);
              }
              warn(&format!(
                "{}: {count} commit{} didn't rebase cleanly — saved to branch {branch} (inspect, then merge/cherry-pick)",
                s.id,
                plural(count)
              ));
              pack_step_diff(root, &session_id, inv, &run, range, daemon_client.as_deref());
            }
            Err(e) => warn(&format!("{}: could not bring in commits — {e}", s.id)),
          }
        }
      }
      if run.ok && !keep_run_dirs() {
        if let Some(clone) = &run.clone_dir {
          let _ = std::fs::remove_dir_all(clone);
        }
      }
    }
  }

  if let Some(reason) = failure.as_deref() {
    for (_, proc) in step_procs.drain() {
      proc.finish_skipped(&format!("not run — workflow stopped after {reason}"));
      skipped_count += 1;
    }
  }
  ui.finish();
  if let Some(msg) = failure {
    fail(&msg);
    return 1;
  }
  if let (Some(from), Some(to)) = (original_caller_tip.as_deref(), caller_tip.as_deref()) {
    pack_job_diff(root, &session_id, from, to);
  }
  ok(&format!(
    "workflow '{}' complete — {ran_count} step{} ran{}",
    def.name,
    plural(ran_count),
    if skipped_count > 0 { format!(", {skipped_count} skipped") } else { String::new() }
  ));
  annotate_run_casts(root, session_skill_casts(&session_id), daemon_client.as_deref(), &mut next_annotate_idx);
  0
}

/// The extra `doctor` preflight: report, as ok/hint lines, which agent images are built and
/// which agents have credentials on this host — a quick "is my setup ready" read before the
/// trivial confirm task runs end to end. Best-effort and non-fatal (the run then proceeds and
/// skips any unavailable route like a normal run).
fn doctor_preflight(rt: &Runtime) {
  let statuses = runtime::image_statuses(&rt.name);
  for agent in config::Harness::ALL {
    let built = statuses.iter().any(|s| s.name == agent.as_str() && s.exists);
    let creds = runtime::check_harness_host(agent).is_ok();
    let img = if built { "image built" } else { "image missing (build it: scsh build-images)" };
    let cred = if creds { "creds present" } else { "creds missing" };
    let line = format!("{}: {img}, {cred}", agent.as_str());
    if built && creds {
      ok(&line);
    } else {
      hint(&line);
    }
  }
}

fn preflight_then(
  action: Action, profile: Option<&str>, verbose: bool, override_yml: Option<&Path>, base: Option<&str>,
) -> i32 {
  // The preflight checks run quietly on success and collapse into one compact
  // summary line (see CONTRIBUTING "Output style"); only failures speak up, each
  // with an actionable ✗/→. A real run is ordered repo-hygiene-first:
  // git → repo → clean → /tmp → config present → config valid → runtime → engine.
  let is_run = matches!(action, Action::Run);

  // git installed → inside a repo → (for a run) clean working tree + gitignored /tmp.
  let root = match preflight_git_repo_clean(is_run) {
    Ok(r) => r,
    Err(code) => return code,
  };

  // Resolve the config: the repo's `.scsh.yml`, an external override bundle
  // (`--override-dot-scsh-yml`), or the global manifest under $SCSH_HOME — the latter two
  // supply skill bodies from their sibling `.skills/`.
  let (cfg, override_skills_root) = match resolve_config_for_run(&root, override_yml, profile) {
    Ok(v) => v,
    Err(code) => return code,
  };

  // Resolve the base before any image or container work: a bad ref costs one git
  // command here instead of a whole fleet running against the wrong (or an empty) range.
  let base = match base.map(|spec| resolve_run_base(&root, spec)).transpose() {
    Ok(b) => b,
    Err(e) => {
      fail(&e);
      hint(&format!("name a commit this repo already has, e.g. {}", bold("--base origin/main")));
      return 1;
    }
  };

  // a container runtime is available and, for a run, its engine is up.
  let rt = match preflight_runtime_engine(is_run) {
    Ok(rt) => rt,
    Err(code) => return code,
  };

  match action {
    Action::List => {
      ok(&preflight_summary(&root, &cfg, &rt));
      list_skills(&cfg, &rt, &root, verbose)
    }
    Action::Run => {
      // Every requested --profile must be `default` (the no-profile skills) or a profile
      // this config declares.
      if profile.is_some() {
        let declared = declared_profiles(&cfg);
        let unknown: Vec<String> =
          requested_profiles(profile).into_iter().filter(|p| p != "default" && !declared.contains(p)).collect();
        if !unknown.is_empty() {
          fail(&format!("unknown profile{}: {}", plural(unknown.len()), unknown.join(", ")));
          let mut avail = vec!["default".to_string()];
          avail.extend(declared.iter().map(|s| s.to_string()));
          hint(&format!("available: {} (see them with: scsh list)", avail.join(", ")));
          return 1;
        }
      }
      let mut selected = select_invocations(&cfg, profile);
      if selected.is_empty() {
        let scope = profile.unwrap_or("default");
        fail(&format!("nothing to run \u{2014} the '{scope}' profile is empty"));
        hint("see the available profiles and their skills:  scsh list");
        hint("then pick one:  scsh run --profile <name>");
        return 1;
      }
      // External override: inject each skill's SKILL.md from the override bundle into the
      // run clone (same path as `scsh run --def`), so the target repo need not ship `.skills/`.
      if let Some(skills_root) = &override_skills_root {
        if let Err(e) = attach_override_skill_bodies(&mut selected, skills_root) {
          fail(&e);
          return 1;
        }
      }
      // Every git/repo/state check passed — one compact line, then the run.
      let prof = profile.map(|p| format!(" · profile {p}")).unwrap_or_default();
      let via = override_yml.map(|p| format!(" · override {}", p.display())).unwrap_or_default();
      ok(&format!("git · repo {} · clean · /tmp ignored{prof}{via}", display_path(&root)));
      // Skip skills whose harness or explicit opencode model is unavailable; fail only when none remain.
      let model_probe = runtime::OpencodeModelProbe::for_selected(&selected);
      let mut runnable: Vec<&ResolvedInvocation> = Vec::new();
      for skill in &selected {
        if let Err(msg) = runtime::check_skill_host(skill.harness, skill.model.as_deref(), &model_probe) {
          warn(&format!("skipping '{}' — {msg}", skill.name));
          continue;
        }
        // Normal runs still require the skill body in the repo; override runs carry theirs
        // for a global in-container install.
        if matches!(skill.delivery, config::SkillDelivery::Repo) {
          let skill_md = root.join(".skills").join(&skill.skill_source).join("SKILL.md");
          if !skill_md.is_file() {
            fail(&format!(
              "skill source missing: .skills/{}/SKILL.md (invocation '{}')",
              skill.skill_source, skill.name
            ));
            return 1;
          }
        }
        runnable.push(skill);
      }
      if runnable.is_empty() {
        fail("no skills to run — every selected skill was skipped (harness or model unavailable on this host)");
        hint("see DEMO.md step 1 — probe add-opencode-gpt-5.4-mini-fast and add-claude-sonnet-4-6");
        return 1;
      }
      let session_id = daemon::new_session_id();
      build_and_run(&rt, &root, &runnable, profile, &session_id, "profile", base.as_ref())
    }
  }
}

/// Load the config for a normal or override-driven run/list.
/// Returns `(config, Some(override_bundle_root))` when the config comes from outside the
/// repo — the bundle root is the parent of the yml (sibling `.skills/` lives there).
///
/// Resolution order: an explicit `--override-dot-scsh-yml` wins; otherwise the repo's own
/// `.scsh.yml` when it declares every requested profile; otherwise the GLOBAL manifest at
/// `$SCSH_HOME/.scsh.yml` (installed by `scsh installskills --global`) when it does. So a
/// repo's config always beats the global one for the profiles it declares, and any git repo
/// can run a globally-installed profile without a local install.
fn resolve_config_for_run(
  root: &Path, override_yml: Option<&Path>, profile: Option<&str>,
) -> Result<(config::Config, Option<PathBuf>), i32> {
  if let Some(override_path) = override_yml {
    let yml = match resolve_override_yml(override_path) {
      Ok(p) => p,
      Err(e) => {
        fail(&e);
        return Err(1);
      }
    };
    let bundle = yml.parent().unwrap_or(Path::new(".")).to_path_buf();
    return Ok((load_validated_yml(&yml)?, Some(bundle)));
  }

  let cfg_path = root.join(".scsh.yml");
  let repo_cfg = if cfg_path.is_file() { Some(load_validated_yml(&cfg_path)?) } else { None };
  // The named profiles the repo config would have to declare ("default" is always the
  // repo's own — the global manifest never hijacks a bare `scsh run`/`scsh list`).
  let wanted: Vec<String> = requested_profiles(profile).into_iter().filter(|p| p != "default").collect();
  if let Some(cfg) = &repo_cfg {
    let declared = declared_profiles(cfg);
    if wanted.is_empty() || wanted.iter().all(|w| declared.contains(w)) {
      return Ok((repo_cfg.unwrap(), None));
    }
  }

  // Global fallback: adopt $SCSH_HOME/.scsh.yml when it declares every requested profile
  // (or when the repo has no .scsh.yml at all). It acts exactly like an override bundle:
  // skill bodies come from its sibling .skills/, and the target repo stays untouched.
  let global_yml = runtime::scsh_home().join(".scsh.yml");
  if global_yml.is_file() {
    let global_cfg = load_validated_yml(&global_yml)?;
    let declared = declared_profiles(&global_cfg);
    if (!wanted.is_empty() && wanted.iter().all(|w| declared.contains(w))) || repo_cfg.is_none() {
      let what = if repo_cfg.is_none() {
        "no .scsh.yml in this repo".to_string()
      } else {
        format!(
          "profile{} {} not in this repo's .scsh.yml",
          plural(wanted.len()),
          wanted.iter().map(|w| format!("'{w}'")).collect::<Vec<_>>().join(", ")
        )
      };
      ok(&format!("{what} — using the global manifest {}", display_path(&global_yml)));
      let bundle = global_yml.parent().unwrap_or(Path::new(".")).to_path_buf();
      return Ok((global_cfg, Some(bundle)));
    }
  }

  match repo_cfg {
    // The repo config lacks a requested profile and the global manifest can't serve it
    // either — return the repo's so the caller's unknown-profile report lists what exists.
    Some(cfg) => Ok((cfg, None)),
    None => {
      fail(".scsh.yml not found — this repository isn't set up for scsh yet");
      hint(&format!("get a ready-to-run project in one command: {}", bold("scsh init-demo-project")));
      hint("(writes .scsh.yml, gitignores /tmp, scaffolds example skills, and commits)");
      hint(&format!(
        "or install the skills once, machine-wide: {} (then any repo can run their profiles)",
        bold("scsh installskills --global")
      ));
      Err(1)
    }
  }
}

/// Absolute path to an existing override `.scsh.yml`.
fn resolve_override_yml(path: &Path) -> Result<PathBuf, String> {
  let p = if path.is_absolute() {
    path.to_path_buf()
  } else {
    std::env::current_dir().map_err(|e| format!("could not resolve cwd for --override-dot-scsh-yml: {e}"))?.join(path)
  };
  if !p.is_file() {
    return Err(format!("--override-dot-scsh-yml path not found: {}", p.display()));
  }
  Ok(p)
}

/// Read + schema-validate a `.scsh.yml` at `yml_path`, reporting failures like the normal preflight.
fn load_validated_yml(yml_path: &Path) -> Result<config::Config, i32> {
  let src = match std::fs::read_to_string(yml_path) {
    Ok(s) => s,
    Err(e) => {
      fail(&format!("could not read {}: {e}", yml_path.display()));
      return Err(1);
    }
  };
  match config::validate(&src) {
    Ok(c) => Ok(c),
    Err(errs) => {
      let n = errs.len();
      fail(&format!("{} does not match the schema ({n} problem{})", yml_path.display(), if n == 1 { "" } else { "s" }));
      for e in &errs {
        hint(e);
      }
      hint("fix the file to match the schema (see 'scsh --help' or the README)");
      Err(1)
    }
  }
}

/// Attach each invocation's `SKILL.md` body from `skills_root/.skills/<skill_source>/SKILL.md`,
/// to be installed GLOBALLY in the container: the harness's own user-level skills directory
/// where the CLI discovers skills natively (claude, cursor), a neutral container path
/// otherwise. The target repo never ships, and its checkout never contains, the skill.
fn attach_override_skill_bodies(invocations: &mut [ResolvedInvocation], skills_root: &Path) -> Result<(), String> {
  for inv in invocations {
    let path = skills_root.join(".skills").join(&inv.skill_source).join("SKILL.md");
    let body =
      std::fs::read_to_string(&path).map_err(|e| format!("override skill missing at {}: {e}", path.display()))?;
    inv.delivery = config::SkillDelivery::GlobalInstall(body);
  }
  Ok(())
}

/// A session's SKILL recordings (its `sessions/<id>/casts/` minus `build-*` image-build
/// casts) — the set worth annotating after a run. The dir is per-session, so everything in
/// it belongs to this run; no before/after snapshot dance needed.
fn session_skill_casts(session_id: &str) -> Vec<std::path::PathBuf> {
  std::fs::read_dir(runtime::session_casts_dir(session_id))
    .map(|entries| {
      entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "cast"))
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("build-")))
        .collect()
    })
    .unwrap_or_default()
}

/// Compact one-line preflight summary for `list` (no run-only guards).
fn preflight_summary(root: &Path, cfg: &config::Config, rt: &Runtime) -> String {
  let names = cfg.skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ");
  let n = cfg.skills.len();
  format!(
    "git · repo {} · .scsh.yml valid ({n} skill{}: {names}) · using {}",
    display_path(root),
    if n == 1 { "" } else { "s" },
    backend_name(&rt.name)
  )
}

/// Friendly name for the chosen containerization backend, for the "using …" line.
/// Apple's runtime shows as "Apple Containers"; docker/podman stay lowercase.
fn backend_name(runtime: &str) -> &str {
  match runtime {
    "docker" => "docker",
    "podman" => "podman",
    "container" => "Apple Containers",
    other => other,
  }
}

/// Abbreviate a path with `~` for `$HOME` (so a repo reads as `~/1`, not a long path).
fn display_path(p: &Path) -> String {
  if let Some(home) = std::env::var_os("HOME") {
    if let Ok(rest) = p.strip_prefix(PathBuf::from(home)) {
      return if rest.as_os_str().is_empty() { "~".to_string() } else { format!("~/{}", rest.display()) };
    }
  }
  p.display().to_string()
}

/// Repo-relative paths with uncommitted changes — staged, unstaged, or untracked
/// (gitignored paths are excluded by git, so `/tmp`, `target/`, etc. never count).
/// scsh runs each skill on a clone of committed state, so a non-empty result means
/// the working tree and that clone would differ; a real run refuses until it is
/// clean. Parsed from `git status --porcelain`, so every kind of change is caught.
fn uncommitted_changes(root: &std::path::Path) -> Vec<String> {
  let status = match git_capture(root, &["status", "--porcelain"]) {
    Some(s) => s,
    None => return Vec::new(),
  };
  let mut out: Vec<String> = Vec::new();
  for line in status.lines() {
    if line.len() < 4 {
      continue;
    }
    // Porcelain is "XY <path>"; a rename shows "old -> new" — take the new path.
    let mut path = line[3..].trim();
    if let Some(idx) = path.find(" -> ") {
      path = &path[idx + 4..];
    }
    let path = path.trim_matches('"');
    if !path.is_empty() && !out.iter().any(|p| p == path) {
      out.push(path.to_string());
    }
  }
  out
}

/// Whether the repository ignores `/tmp` (a `/tmp` line in .gitignore makes the
/// repo-root path `tmp` ignored). Checked via `git check-ignore` so every
/// gitignore source is honored.
fn tmp_is_gitignored(root: &std::path::Path) -> bool {
  git_command().arg("-C").arg(root).args(["check-ignore", "-q", "tmp"]).status().map(|s| s.success()).unwrap_or(false)
}

/// `scsh list` / `scsh ls` — the inventory: every skill grouped by profile (the reserved
/// `default` profile is the skills with no `profile:`), each with its result file, commit
/// flag, and the env it needs. `--verbose` additionally prints the generated Dockerfile and
/// the exact per-skill build/run commands.
fn list_skills(cfg: &config::Config, rt: &Runtime, root: &std::path::Path, verbose: bool) -> i32 {
  let expanded = config::expand_invocations(cfg);
  println!();
  println!(
    "{} {}",
    h_head("Profiles & skills"),
    h_dim(&format!(
      "\u{2014} {} invocation{} · run one with `scsh run --profile <name>`",
      expanded.len(),
      plural(expanded.len())
    ))
  );
  let mut groups: Vec<(String, Vec<&ResolvedInvocation>)> =
    vec![("default".to_string(), expanded.iter().filter(|s| s.profile.is_none()).collect())];
  for p in declared_profiles(cfg) {
    if p == "default" {
      continue;
    }
    groups.push((p.clone(), expanded.iter().filter(|s| s.profile.as_deref() == Some(p.as_str())).collect()));
  }
  for (name, members) in &groups {
    if members.is_empty() {
      let note = if name == "default" { "\u{2014} empty (a bare `scsh run` is a no-op)" } else { "\u{2014} empty" };
      println!("  {} {}", h_head(&format!("{name} (0)")), h_dim(note));
      continue;
    }
    let how = if name == "default" { "scsh run".to_string() } else { format!("scsh run --profile {name}") };
    println!("  {} {}", h_head(&format!("{name} ({})", members.len())), h_dim(&format!("\u{2014} {how}")));
    for s in members {
      let mut notes = String::new();
      if s.commits {
        notes.push_str("  \u{b7} commits back");
      }
      let env: Vec<&str> = s.env.iter().map(|e| e.key.as_str()).collect();
      if !env.is_empty() {
        notes.push_str(&format!("  \u{b7} env: {}", env.join(", ")));
      }
      help_row(&s.name, &format!("\u{2192} {}{notes}", s.result));
    }
  }

  if verbose {
    let skills = &expanded[..];
    let (uid, gid) = runtime::host_ids();
    // Show / fingerprint the same Dockerfile text the runtime will build (Apple: compacted).
    let df = runtime::dockerfile_for_runtime(&rt.name);
    let mut harnesses: std::collections::BTreeSet<config::Harness> = std::collections::BTreeSet::new();
    for s in skills.iter() {
      harnesses.insert(s.harness);
    }
    println!("\n{}", h_head("Images"));
    if rt.name == "container" {
      println!(
        "{}",
        h_dim("--- Dockerfile for Apple Containers (comments stripped; gRPC header ≤16KB, apple/container#735) ---")
      );
    } else {
      println!("{}", h_dim("--- generated Dockerfile (in memory; shared base + per-harness targets) ---"));
    }
    print!("{df}");
    let host_tz = runtime::host_timezone();
    let base_fp = runtime::base_image_fingerprint(&df, uid, gid, &host_tz);
    println!("--- build {} first (shared toolchain; agent uid={uid} gid={gid}) ---", runtime::BASE_IMAGE_TARGET);
    print_build_command(
      &rt.name,
      runtime::BASE_IMAGE_TAG,
      runtime::BASE_IMAGE_TARGET,
      &df,
      uid,
      gid,
      &host_tz,
      &base_fp,
    );
    let specs: Vec<runtime::ImageBuildSpec> =
      harnesses.iter().map(|h| runtime::image_build_spec(*h, &df, uid, gid, &host_tz)).collect();
    for spec in &specs {
      println!("--- build {} (harness layer on top of {}) ---", spec.target, runtime::BASE_IMAGE_TARGET);
      print_build_command(&rt.name, &spec.tag, &spec.target, &df, uid, gid, &host_tz, &spec.fingerprint);
    }
    println!("\n{}", h_head("Per-skill commands"));
    for skill in skills {
      let name = runtime::run_dir_name(now_secs(), &skill.name, &rt.name);
      let run_dir = format!("/tmp/{name}");
      let tag = runtime::image_tag(skill.harness);
      let cmd = runtime::harness_command(
        skill.harness,
        skill.model.as_deref(),
        skill.effort.as_deref(),
        &skill.skill_source,
        &skill.result,
        skill.terminal,
        &skill.delivery,
      );
      let model = skill.model.as_deref().unwrap_or("(harness default)");
      let timeout = skill.timeout.map(|t| format!("{t}s")).unwrap_or_else(|| "none".into());
      println!(
        "\n[{}]  skill={}  harness={}  model={model}  timeout={timeout}",
        skill.name,
        skill.skill_source,
        skill.harness.as_str()
      );
      if runtime::uses_git_transport(&rt.name) {
        println!("  push:  git push {run_dir}/{} HEAD refs/remotes/origin/*", runtime::TRANSPORT_BARE);
        println!(
          "  run:   container clones git://<gateway>:<port>/{} (gateway from ip route; port in SCSH_GIT_PORT)",
          runtime::TRANSPORT_BARE
        );
      } else {
        println!("  clone: {}", runtime::shell_join(&runtime::clone_command(&root.to_string_lossy(), &run_dir)));
      }
      match resolve_env(&skill.env) {
        Ok(env) => {
          let vols: Vec<(String, String)> = runtime::harness_volumes(skill.harness);
          let vol_refs: Vec<(&str, &str)> = vols.iter().map(|(h, m)| (h.as_str(), m.as_str())).collect();
          let repo_mount = if runtime::uses_git_transport(&rt.name) {
            runtime::RepoMountMode::TmpOnly
          } else {
            runtime::RepoMountMode::Full
          };
          let run = runtime::run_command(&rt.name, &tag, &run_dir, &name, &env, &vol_refs, &cmd, repo_mount);
          println!("  run:   {}", runtime::shell_join(&run));
        }
        Err(message) => println!("  run:   (skill would be REFUSED before running — {message})"),
      }
      println!("  after: require '{}', then copy it back into the repo (backing up any existing file)", skill.result);
    }
  } else {
    println!("{}", h_dim("  run `scsh list --verbose` to also see the image Dockerfile and exact commands"));
  }
  println!();
  0
}

// ---------------------------------------------------------------------------
// Programmatic profile inspection (runtime-free): `list --json` + `check-profile`
//
// These let another tool discover and gate on profiles without scraping the human
// listing and without a container runtime — they only need git, a repo, and a
// schema-valid .scsh.yml. Errors go to stderr (✗/→) so stdout stays machine-clean.
// ---------------------------------------------------------------------------

/// Load and schema-validate the repo's `.scsh.yml` for the read-only inspection commands —
/// the same git → repo → present → valid chain as a run's preflight, but WITHOUT the
/// container-runtime/engine checks, so profiles can be queried on any machine. On failure it
/// reports the problem and returns the process exit code; stdout is left untouched.
fn load_config_for_inspection(override_yml: Option<&Path>, profile: Option<&str>) -> Result<config::Config, i32> {
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return Err(1);
  }
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return Err(1);
    }
  };
  let (cfg, _) = resolve_config_for_run(&root, override_yml, profile)?;
  Ok(cfg)
}

/// The config's profiles as `(name, skill-names)`: the reserved `default` profile (the
/// no-`profile:` skills) first, then each declared profile in first-seen order.
fn profile_groups(cfg: &config::Config) -> Vec<(String, Vec<String>)> {
  let expanded = config::expand_invocations(cfg);
  let mut groups: Vec<(String, Vec<String>)> =
    vec![("default".to_string(), expanded.iter().filter(|s| s.profile.is_none()).map(|s| s.name.clone()).collect())];
  for p in declared_profiles(cfg) {
    if p == "default" {
      continue;
    }
    let members =
      expanded.iter().filter(|s| s.profile.as_deref() == Some(p.as_str())).map(|s| s.name.clone()).collect();
    groups.push((p, members));
  }
  groups
}

/// `scsh list --json` — every profile and its skills as machine-readable JSON on stdout, so
/// another tool can discover them without scraping the human listing (or needing a runtime).
/// The reserved `default` profile is always present (possibly empty); every other profile
/// listed has at least one skill. Stable shape:
/// `{"profiles":[{"name":"default","skills":["add"]}, …]}`.
fn list_profiles_json(override_yml: Option<&Path>) -> i32 {
  let cfg = match load_config_for_inspection(override_yml, None) {
    Ok(c) => c,
    Err(code) => return code,
  };
  let groups = profile_groups(&cfg);
  let mut out = String::from("{\n  \"profiles\": [\n");
  for (i, (name, skills)) in groups.iter().enumerate() {
    let names = skills.iter().map(|s| json::quote(s)).collect::<Vec<_>>().join(", ");
    out.push_str(&format!("    {{ \"name\": {}, \"skills\": [{}] }}", json::quote(name), names));
    out.push_str(if i + 1 < groups.len() { ",\n" } else { "\n" });
  }
  out.push_str("  ]\n}");
  println!("{out}");
  0
}

/// `scsh check-profile <name>` — a runtime-free existence check for scripts. Exit 0 iff the
/// profile exists AND has at least one skill (so a caller can gate on it directly); non-zero
/// otherwise. The reserved `default` profile "exists" only when some skill has no `profile:`.
/// Prints a one-line ✓/✗ — the exit code is the contract, so redirect it when scripting.
fn check_profile_cmd(profile: Option<&str>, override_yml: Option<&Path>) -> i32 {
  let name = match profile {
    Some(p) => p,
    None => {
      fail("check-profile needs a profile name, e.g. scsh check-profile multiply");
      return 2;
    }
  };
  let cfg = match load_config_for_inspection(override_yml, Some(name)) {
    Ok(c) => c,
    Err(code) => return code,
  };
  let count = select_invocations(&cfg, Some(name)).len();
  if count > 0 {
    ok(&format!("profile '{name}' has {count} skill{}", plural(count)));
    return 0;
  }
  if name == "default" || declared_profiles(&cfg).iter().any(|p| p == name) {
    fail(&format!("profile '{name}' exists but has no skills"));
  } else {
    fail(&format!("no such profile '{name}'"));
    let mut avail = declared_profiles(&cfg);
    if !avail.iter().any(|p| p == "default") {
      avail.insert(0, "default".to_string());
    }
    hint(&format!("available: {}", avail.join(", ")));
  }
  1
}

/// `scsh probe [profile…]` — which harness·model routes are runnable on THIS host: the agent
/// CLI is installed and authenticated, and an explicit opencode model is actually listed by
/// `opencode models`. No image is built and no container starts. With profiles, only those
/// profiles' skills are probed; without, every invocation in the resolved config is. Routes
/// are deduped across skills (five reviewers sharing three routes probe as three), and the
/// config resolves exactly as for a run — `--override-dot-scsh-yml`, then the repo's
/// `.scsh.yml`, then the global manifest. Exit 0 when at least one probed route is available
/// and 1 when none is, so scripts and skills gate a fleet directly:
/// `scsh probe code-review && scsh run code-review`.
fn probe_cmd(profile: Option<&str>, override_yml: Option<&Path>, json_flag: bool) -> i32 {
  let cfg = match load_config_for_inspection(override_yml, profile) {
    Ok(c) => c,
    Err(code) => return code,
  };
  let selected: Vec<config::ResolvedInvocation> =
    if profile.is_some() { select_invocations(&cfg, profile) } else { config::expand_invocations(&cfg) };
  if selected.is_empty() {
    match profile {
      Some(p) => fail(&format!("nothing to probe — the '{p}' profile is empty")),
      None => fail("nothing to probe — the config declares no skills"),
    }
    hint("see the available profiles and their skills:  scsh list");
    return 2;
  }

  // Distinct harness·model routes, first-seen order; each is probed once.
  let model_probe = runtime::OpencodeModelProbe::for_selected(&selected);
  let mut routes: Vec<(config::Harness, Option<String>)> = Vec::new();
  for inv in &selected {
    let key = (inv.harness, inv.model.clone());
    if !routes.contains(&key) {
      routes.push(key);
    }
  }
  let rows: Vec<(&'static str, Option<String>, Result<(), String>)> = routes
    .iter()
    .map(|(harness, model)| {
      (harness.as_str(), model.clone(), runtime::check_skill_host(*harness, model.as_deref(), &model_probe))
    })
    .collect();
  let available = rows.iter().filter(|(_, _, res)| res.is_ok()).count();

  if json_flag {
    let mut out = String::from("{\n  \"routes\": [\n");
    for (i, (harness, model, res)) in rows.iter().enumerate() {
      out.push_str(&format!("    {{ \"harness\": {}, ", json::quote(harness)));
      match model {
        Some(m) => out.push_str(&format!("\"model\": {}, ", json::quote(m))),
        None => out.push_str("\"model\": null, "),
      }
      match res {
        Ok(()) => out.push_str("\"available\": true }"),
        Err(e) => out.push_str(&format!("\"available\": false, \"reason\": {} }}", json::quote(e))),
      }
      out.push_str(if i + 1 < rows.len() { ",\n" } else { "\n" });
    }
    out.push_str(&format!("  ],\n  \"available\": {available},\n  \"total\": {}\n}}", rows.len()));
    println!("{out}");
  } else {
    for (harness, model, res) in &rows {
      let route = match model {
        Some(m) => format!("{harness} · {m}"),
        None => (*harness).to_string(),
      };
      match res {
        Ok(()) => ok(&route),
        Err(e) => fail(&format!("{route} — {e}")),
      }
    }
    let summary = format!("{available} of {} route{} available", rows.len(), plural(rows.len()));
    if available > 0 {
      ok(&summary);
    } else {
      fail(&summary);
      hint("log in to at least one agent CLI on this host, then re-run");
    }
  }
  if available > 0 {
    0
  } else {
    1
  }
}

fn daemon_cmd(action: DaemonAction) -> i32 {
  match action {
    DaemonAction::Start => match daemon::start_persistent() {
      Ok(()) => {
        ok(&format!("session browser daemon listening on {}", daemon::base_url(daemon::daemon_port())));
        0
      }
      Err(e) => {
        fail(&format!("could not start daemon: {e}"));
        hint("→ check SCSH_DAEMON_PORT and whether another process is already listening on that port");
        1
      }
    },
    DaemonAction::Stop => match daemon::stop() {
      Ok(true) => {
        ok("session browser daemon stopped");
        0
      }
      Ok(false) => {
        fail("session browser daemon is not running");
        hint("→ start it with: scsh daemon start");
        1
      }
      Err(e) => {
        fail(&format!("could not stop daemon: {e}"));
        hint("→ check SCSH_DAEMON_PORT and stale files under $TMPDIR/scsh-daemon/");
        1
      }
    },
    DaemonAction::Restart => {
      let _ = daemon::stop();
      match daemon::start_persistent() {
        Ok(()) => {
          ok(&format!("session browser daemon restarted on {}", daemon::base_url(daemon::daemon_port())));
          0
        }
        Err(e) => {
          fail(&format!("could not restart daemon: {e}"));
          hint("→ check SCSH_DAEMON_PORT and whether another process is listening on that port");
          1
        }
      }
    }
    DaemonAction::Status => {
      let port = daemon::daemon_port();
      if daemon::Client::daemon_alive() {
        // The RUNNING daemon's version, which can lag the installed binary until a
        // restart. An older daemon serving "unknown" (pre-endpoint) is reported honestly.
        let running =
          daemon::daemon_reported_version(port).unwrap_or_else(|| "unknown (older than this feature)".into());
        let where_at = daemon::base_url(port);
        match daemon::read_live_pid(port) {
          Some(pid) => ok(&format!("session browser daemon running (pid {pid}, scsh {running}) on {where_at}")),
          None => ok(&format!("session browser daemon responding (scsh {running}) on {where_at}")),
        }
        // Nudge only on a real mismatch: the binary was upgraded but the daemon still runs
        // the old code, so a restart is needed to pick up the new build.
        let installed = crate::version::display();
        if daemon_version_is_stale(&running, &installed) {
          hint(&format!("→ the installed scsh is {installed}; restart to run it in the daemon: scsh daemon restart"));
        }
        0
      } else if let Some(pid) = daemon::read_live_pid(port) {
        fail(&format!("session browser daemon pid {pid} exists but is not responding on {}", daemon::base_url(port)));
        hint("→ recover with: scsh daemon restart");
        1
      } else {
        fail("session browser daemon is not running");
        hint("→ start it with: scsh daemon start");
        1
      }
    }
  }
}

/// Whether the running daemon's reported version warrants a "restart to pick up the new
/// build" nudge: it differs from the installed binary AND is a real version (a daemon too
/// old to serve `/api/v1/version` reports `unknown …`, which is not actionable via restart
/// of *this* build — we already say so in the status line, without a misleading nudge).
fn daemon_version_is_stale(running: &str, installed: &str) -> bool {
  running != installed && !running.starts_with("unknown")
}

fn daemon_serve(mode: daemon::DaemonMode, port: u16) -> i32 {
  let server = daemon::Server::new(mode, port);
  match server.run() {
    Ok(()) => 0,
    Err(e) => {
      fail(&format!("session browser daemon exited: {e}"));
      hint("→ check SCSH_DAEMON_PORT and logs from the child process");
      1
    }
  }
}

/// `scsh failures`: render the JSONL failure log, filtered and optionally aggregated.
fn failures_cmd(opts: &FailuresOpts) -> i32 {
  let mut events = failure::read_events();
  if let Some(s) = &opts.session {
    events.retain(|e| e.session.as_deref() == Some(s.as_str()));
  }
  if let Some(s) = &opts.skill {
    events.retain(|e| e.skill.as_deref() == Some(s.as_str()));
  }
  if let Some(r) = &opts.reason {
    events.retain(|e| e.reason == *r);
  }
  if events.is_empty() {
    println!("no recorded failures match");
    hint(&format!("failure log: {}", failure::log_path().display()));
    return 0;
  }
  if opts.stats {
    print_failure_stats(&events);
    return 0;
  }
  let keep = match opts.last {
    Some(0) => events.len(),
    Some(n) => n,
    None => 50,
  };
  let start = events.len().saturating_sub(keep);
  if start > 0 {
    println!("… {start} earlier event(s) hidden — rerun with --last 0 for all");
  }
  for e in &events[start..] {
    print_failure_event(e);
  }
  0
}

fn print_failure_event(e: &failure::FailureEvent) {
  let when = runtime::format_utc_timestamp(e.ts);
  if e.kind == "run_summary" {
    let profile = e.profile.as_deref().unwrap_or("(no profile)");
    let session = e.session.as_deref().unwrap_or("?");
    println!(
      "{when}  run failed: {}/{} skills (profile {profile}, session {session})",
      e.failed.unwrap_or(0),
      e.total.unwrap_or(0)
    );
    return;
  }
  let mut parts = Vec::new();
  if let Some(s) = &e.session {
    parts.push(format!("session={s}"));
  }
  if let Some(s) = &e.skill {
    parts.push(format!("skill={s}"));
  }
  if let Some(s) = &e.subject {
    parts.push(format!("proc={s}"));
  }
  if let Some(h) = &e.harness {
    let model = e.model.as_deref().unwrap_or("(harness default)");
    parts.push(format!("route={h}·{model}"));
  }
  let verb = if e.kind == "retry" { "retried" } else { "failed" };
  println!("{when}  [{}] {verb}  {}", e.reason, parts.join(" "));
  if let Some(d) = &e.detail {
    for line in d.lines() {
      println!("    {line}");
    }
  }
}

/// `scsh failures --stats`: failures and retries per harness·model route, then per reason.
fn print_failure_stats(events: &[failure::FailureEvent]) {
  use std::collections::BTreeMap;
  // Route → (failure count, retry count, reason → count). Only events that carry a route.
  let mut routes: BTreeMap<String, (usize, usize, BTreeMap<String, usize>)> = BTreeMap::new();
  let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
  for e in events {
    if e.kind == "run_summary" {
      continue;
    }
    *reasons.entry(e.reason.clone()).or_default() += 1;
    if let Some(h) = &e.harness {
      let route = format!("{h} · {}", e.model.as_deref().unwrap_or("(harness default)"));
      let entry = routes.entry(route).or_default();
      if e.kind == "retry" {
        entry.1 += 1;
      } else {
        entry.0 += 1;
        *entry.2.entry(e.reason.clone()).or_default() += 1;
      }
    }
  }
  if routes.is_empty() {
    println!("no route-attributed failures recorded yet (routes appear on failed skill events)");
  } else {
    println!("failures by route (harness · model):");
    for (route, (fails, retries, by_reason)) in &routes {
      let mut reason_bits: Vec<String> = by_reason.iter().map(|(r, n)| format!("{r} ×{n}")).collect();
      reason_bits.sort();
      let retry_note = if *retries > 0 { format!(", {retries} retried") } else { String::new() };
      println!("  {route}: {fails} failure(s){retry_note} — {}", reason_bits.join(", "));
    }
  }
  println!();
  println!("failures by reason (all events):");
  for (reason, n) in &reasons {
    println!("  {reason}: {n}");
  }
}

/// `scsh stats`: aggregate the durable run statistics — how long skills take per
/// harness·model route, against the workload they processed (commits + LOC over main).
fn stats_cmd(opts: &FailuresOpts, profile: Option<&str>) -> i32 {
  let records = stats::read_records();
  let matches_common = |r: &stats::StatRecord| {
    if let Some(s) = &opts.session {
      if r.session != *s {
        return false;
      }
    }
    if let Some(p) = profile {
      if r.profile.as_deref() != Some(p) {
        return false;
      }
    }
    true
  };
  let skill_rows: Vec<&stats::StatRecord> = records
    .iter()
    .filter(|r| r.kind == "skill" && matches_common(r))
    .filter(|r| {
      opts.skill.as_deref().is_none_or(|s| r.skill.as_deref() == Some(s) || r.skill_source.as_deref() == Some(s))
    })
    .filter(|r| opts.harness.as_deref().is_none_or(|h| r.harness.as_deref() == Some(h)))
    .filter(|r| opts.model.as_deref().is_none_or(|m| r.model.as_deref() == Some(m)))
    .collect();
  let run_rows: Vec<&stats::StatRecord> = records.iter().filter(|r| r.kind == "run" && matches_common(r)).collect();
  if skill_rows.is_empty() && run_rows.is_empty() {
    println!("no recorded runs match");
    hint(&format!("stats file: {}", stats::stats_path().display()));
    return 0;
  }
  if opts.raw {
    print_stats_raw(&skill_rows, &run_rows, opts.last);
    return 0;
  }
  print_run_aggregates(&run_rows);
  print_skill_aggregates(&skill_rows);
  hint(&format!("stats file: {} (individual rows: scsh stats --raw)", stats::stats_path().display()));
  0
}

fn print_stats_raw(skill_rows: &[&stats::StatRecord], run_rows: &[&stats::StatRecord], last: Option<usize>) {
  let mut rows: Vec<&stats::StatRecord> = skill_rows.iter().chain(run_rows.iter()).copied().collect();
  rows.sort_by_key(|r| r.ts);
  let keep = match last {
    Some(0) => rows.len(),
    Some(n) => n,
    None => 50,
  };
  let start = rows.len().saturating_sub(keep);
  if start > 0 {
    println!("… {start} earlier row(s) hidden — rerun with --last 0 for all");
  }
  for r in &rows[start..] {
    let when = runtime::format_utc_timestamp(r.ts);
    if r.kind == "run" {
      println!(
        "{when}  run    {:>7.1}s  profile={} session={} skills={}/{} ok  commits={} loc={}",
        r.duration_secs,
        r.profile.as_deref().unwrap_or("(default)"),
        r.session,
        r.skills_total.unwrap_or(0) - r.skills_failed.unwrap_or(0),
        r.skills_total.unwrap_or(0),
        r.commits,
        r.loc_total(),
      );
    } else {
      let route = r.route_label();
      let outcome = r.outcome.as_deref().unwrap_or("?");
      let retry = if r.attempts > 1 { " (retried)" } else { "" };
      println!(
        "{when}  skill  {:>7.1}s  {}  {route}  {outcome}{retry}  commits={} loc={}",
        r.duration_secs,
        r.skill_source.as_deref().or(r.skill.as_deref()).unwrap_or("?"),
        r.commits,
        r.loc_total(),
      );
    }
  }
}

fn print_run_aggregates(run_rows: &[&stats::StatRecord]) {
  if run_rows.is_empty() {
    return;
  }
  use std::collections::BTreeMap;
  let mut by_profile: BTreeMap<String, Vec<&stats::StatRecord>> = BTreeMap::new();
  for r in run_rows {
    by_profile.entry(r.profile.clone().unwrap_or_else(|| "(default)".into())).or_default().push(r);
  }
  println!("runs by profile:");
  for (profile, rows) in &by_profile {
    let n = rows.len() as f64;
    let mean_secs: f64 = rows.iter().map(|r| r.duration_secs).sum::<f64>() / n;
    let mean_commits: f64 = rows.iter().map(|r| r.commits as f64).sum::<f64>() / n;
    let mean_loc: f64 = rows.iter().map(|r| r.loc_total() as f64).sum::<f64>() / n;
    let failed_runs = rows.iter().filter(|r| r.skills_failed.unwrap_or(0) > 0).count();
    println!(
      "  {profile}: {} run(s), avg {:.0}s, avg workload {:.1} commits / {:.0} LOC{}",
      rows.len(),
      mean_secs,
      mean_commits,
      mean_loc,
      if failed_runs > 0 { format!(", {failed_runs} with failures") } else { String::new() },
    );
  }
  println!();
}

fn print_skill_aggregates(skill_rows: &[&stats::StatRecord]) {
  if skill_rows.is_empty() {
    return;
  }
  use std::collections::BTreeMap;
  // Group by (skill_source, harness · model (effort)) — "each reviewer, each route".
  let mut groups: BTreeMap<(String, String), Vec<&stats::StatRecord>> = BTreeMap::new();
  for r in skill_rows {
    let skill = r.skill_source.clone().or_else(|| r.skill.clone()).unwrap_or_else(|| "?".into());
    groups.entry((skill, r.route_label())).or_default().push(r);
  }
  let skill_w = groups.keys().map(|(s, _)| s.len()).max().unwrap_or(5).max(5);
  let route_w = groups.keys().map(|(_, r)| r.len()).max().unwrap_or(5).max(5);
  println!("skills by route (durations exclude cache hits):");
  println!(
    "  {:<skill_w$}  {:<route_w$}  {:>4} {:>3} {:>4} {:>5} {:>5}  {:>7} {:>7} {:>7}  {:>8} {:>7}",
    "skill", "route", "runs", "ok", "fail", "cache", "retry", "avg s", "min s", "max s", "~commits", "~LOC"
  );
  for ((skill, route), rows) in &groups {
    let agg = stats::aggregate_skills(rows);
    println!(
      "  {:<skill_w$}  {:<route_w$}  {:>4} {:>3} {:>4} {:>5} {:>5}  {:>7.1} {:>7.1} {:>7.1}  {:>8.1} {:>7.0}",
      skill,
      route,
      agg.runs,
      agg.ok,
      agg.failed,
      agg.cached,
      agg.retried,
      agg.mean_secs,
      agg.min_secs,
      agg.max_secs,
      agg.mean_commits,
      agg.mean_loc,
    );
  }
}

/// `scsh prune`: show the daemon's run-dir cleanup queue; `--now` forces a janitor pass
/// (through the daemon when it is running, else directly on the persisted queue).
fn prune_cmd(now_flag: bool) -> i32 {
  let port = daemon::daemon_port();
  let queue = daemon::prune::PruneQueue::load(port);
  if !now_flag {
    if queue.jobs.is_empty() {
      ok("run-dir prune queue is empty");
      return 0;
    }
    let now = daemon::now_unix_secs();
    println!("{} pending run-dir prune job(s):", queue.jobs.len());
    for j in &queue.jobs {
      let outcome = if j.outcome_ok { "ok" } else { "failed" };
      let when =
        if now >= j.eligible_at { "eligible now".to_string() } else { format!("eligible in {}s", j.eligible_at - now) };
      println!("  {}  ({outcome} run, {when})", j.run_dir);
    }
    hint("delete every eligible dir now with: scsh prune --now");
    return 0;
  }
  let before = queue.jobs.len();
  if daemon::daemon_port_reachable(port) {
    if !daemon::post_once(port, "/api/v1/prune/tick", "{}") {
      fail("session browser daemon is running but rejected the prune request");
      return 1;
    }
  } else {
    // No daemon: run one pass directly on the persisted queue.
    let mut q = queue;
    let _ = q.tick(daemon::now_unix_secs());
    q.save(port);
  }
  let after = daemon::prune::PruneQueue::load(port).jobs.len();
  ok(&format!("prune pass complete: {before} job(s) before, {after} remaining"));
  0
}

/// `scsh gc`: report (default) or delete old `$SCSH_HOME/sessions/` dirs past `--keep` and
/// `--days`. Never touches `projects/`, `stats.jsonl`, or redb files.
fn gc_cmd(opts: &gc::GcOpts) -> i32 {
  let home = runtime::scsh_home();
  let plan = gc::plan(&home, opts, gc::now_unix_secs());
  if plan.candidates.is_empty() {
    ok("nothing to reclaim");
    return 0;
  }
  for c in &plan.candidates {
    println!("  {}  {}", c.path.display(), gc::human_bytes(c.bytes));
  }
  if opts.apply {
    let freed = gc::apply_plan(&plan);
    ok(&format!("deleted {} path(s); freed {}", plan.candidates.len(), gc::human_bytes(freed)));
  } else {
    ok(&format!(
      "reclaimable: {} across {} path(s) (dry-run)",
      gc::human_bytes(plan.total_bytes),
      plan.candidates.len()
    ));
    hint(&format!("delete with: {}", bold("scsh gc --apply")));
  }
  0
}

/// Absolute repo path for the session browser (canonical when possible).
fn repo_path_for_session(root: &Path) -> String {
  daemon::absolutize_repo_path(root)
}

/// Best-effort daemon teardown on every exit path (build failure, skill failure, panic, early return).
struct DaemonSession {
  client: Option<std::sync::Arc<daemon::Client>>,
  ping_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
  registered: bool,
}

impl DaemonSession {
  fn cleanup(&mut self) {
    if let Some(flag) = self.ping_active.take() {
      flag.store(false, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(c) = self.client.take() {
      // This run is over either way, so its stop marker has done its job. Clearing it here —
      // the one teardown both the flat and workflow paths pass through — keeps the
      // cancel-requests dir from collecting a file per stopped job.
      daemon::clear_session_cancel(c.session_id());
      if self.registered {
        c.finish_session();
        ok(&format!("job {}", c.session_url()));
      } else {
        c.flush();
      }
    }
  }
}

impl Drop for DaemonSession {
  fn drop(&mut self) {
    self.cleanup();
  }
}

#[allow(clippy::too_many_arguments)]
fn build_and_run(
  rt: &Runtime, root: &std::path::Path, skills: &[&ResolvedInvocation], profile: Option<&str>, session: &str,
  kind: &str, base: Option<&RunBase>,
) -> i32 {
  ui::signals::install();

  // Session browser daemon — `scsh run` always tries to attach; ephemeral auto-start when
  // needed. The caller minted (or was handed) the session id, so artifacts and the deep link
  // agree on it before anything runs.
  let session_id = session.to_string();
  let mut daemon_session = DaemonSession { client: None, ping_active: None, registered: false };
  match daemon::ensure_for_run() {
    Ok(()) => {
      let client = std::sync::Arc::new(daemon::Client::new(session_id.clone()));
      let skill_meta: Vec<(&str, &str)> = skills.iter().map(|s| (s.name.as_str(), s.harness.as_str())).collect();
      if client.register_session(&repo_path_for_session(root), &current_branch(root), profile, kind, &skill_meta) {
        ok(&format!("track progress at {}", client.session_url()));
        let ping_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ping_flag = std::sync::Arc::clone(&ping_active);
        let ping_client = std::sync::Arc::clone(&client);
        std::thread::spawn(move || {
          while ping_flag.load(std::sync::atomic::Ordering::Relaxed) {
            ping_client.ping();
            std::thread::sleep(Duration::from_secs(2));
          }
        });
        daemon_session.client = Some(client);
        daemon_session.ping_active = Some(ping_active);
        daemon_session.registered = true;
      } else {
        hint(&format!("session browser daemon is up but registration failed; try {}", client.session_url()));
      }
    }
    Err(e) => {
      hint(&format!("session browser daemon unavailable ({e}); continuing without live browser UI"));
    }
  }
  let daemon_client = daemon_session.client.clone();

  let (uid, gid) = runtime::host_ids();
  let secs = now_secs();
  if !keep_run_dirs() {
    let swept = sweep_stale_run_dirs(secs);
    if swept > 0 {
      hint(&format!("swept {swept} stale run dir{} from /tmp", plural(swept)));
    }
  }
  let caller_tip = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());

  let needs_opencode = skills.iter().any(|s| s.harness == config::Harness::Opencode);
  let needs_claude = skills.iter().any(|s| s.harness == config::Harness::Claude);
  let needs_codex = skills.iter().any(|s| s.harness == config::Harness::Codex);
  if needs_opencode && opencode_auth_enabled() && runtime::opencode_auth_ready() {
    ok("opencode creds found (auth.json and opencode config forwarded into the run clone)");
  }
  if needs_claude && runtime::claude_container_auth_ready() {
    let via =
      if runtime::claude_oauth_token().is_some() { "CLAUDE_CODE_OAUTH_TOKEN" } else { "~/.claude/.credentials.json" };
    ok(&format!("claude credentials found ({via} forwarded into claude skills)"));
  }
  if needs_codex && codex_auth_enabled() && runtime::codex_container_auth_ready() {
    let via = if runtime::codex_auth_file_on_host().is_some() { "~/.codex/auth.json" } else { "OPENAI_API_KEY" };
    ok(&format!("codex credentials found ({via} forwarded into codex skills)"));
  }
  let needs_grok = skills.iter().any(|s| s.harness == config::Harness::Grok);
  if needs_grok && grok_auth_enabled() && runtime::grok_container_auth_ready() {
    let via = if runtime::grok_auth_file_on_host().is_some() { "~/.grok/auth.json" } else { "XAI_API_KEY" };
    ok(&format!("grok credentials found ({via} forwarded into grok skills)"));
  }
  let needs_cursor = skills.iter().any(|s| s.harness == config::Harness::Cursor);
  if needs_cursor && cursor_auth_enabled() && runtime::cursor_container_auth_ready() {
    let via = if runtime::cursor_api_key().is_some() {
      "CURSOR_API_KEY"
    } else if runtime::cursor_auth_file_on_host().is_some() {
      "auth.json"
    } else {
      "macOS keychain"
    };
    ok(&format!("cursor credentials found ({via} forwarded into cursor skills)"));
  }

  let ui = ui::screen::LiveUi::new(console::user_attended_stderr(), daemon_client.clone());

  // Apple Containers: comment-strip so the Dockerfile fits the gRPC header limit (#735).
  let df = runtime::dockerfile_for_runtime(&rt.name);
  if rt.name == "container" && runtime::apple_dockerfile_too_large(&df) {
    fail(&runtime::apple_dockerfile_too_large_message(df.len()));
    return 1;
  }
  let tz = runtime::host_timezone();
  // Harness build order: first time each harness appears in the manifest (not enum sort).
  let mut harness_list = Vec::new();
  let mut seen_harness = std::collections::BTreeSet::new();
  for s in skills {
    if seen_harness.insert(s.harness) {
      harness_list.push(s.harness);
    }
  }
  let rt_name = rt.name.clone();
  let base_fp = runtime::base_image_fingerprint(&df, uid, gid, &tz);
  let base_needs_build = !runtime::image_is_up_to_date(&rt_name, runtime::BASE_IMAGE_TAG, &base_fp);
  let mut harness_builds: Vec<runtime::ImageBuildSpec> = Vec::new();
  for &h in &harness_list {
    let spec = runtime::image_build_spec(h, &df, uid, gid, &tz);
    if !runtime::image_is_up_to_date(&rt_name, &spec.tag, &spec.fingerprint) {
      harness_builds.push(spec);
    }
  }
  let any_image_build = base_needs_build || !harness_builds.is_empty();

  // Base image first, then one build proc per harness that actually needs rebuilding;
  // the harness images only depend on the base, so they build in parallel.
  let mut base_build = None;
  if base_needs_build {
    let base_label = format!("using {} · build base", backend_name(&rt.name));
    let builder = format!("{} build", backend_name(&rt.name));
    let p = ui.proc(base_label.clone(), false);
    if let Some(c) = &daemon_client {
      c.proc_add(p.index(), &base_label, daemon::ProcKind::Build, None, Some(&builder), None, None, None, None, None);
    }
    base_build = Some(p);
  }
  let mut harness_build_procs: Vec<ui::screen::Proc> = Vec::with_capacity(harness_builds.len());
  for spec in &harness_builds {
    let label = format!("using {} · build {}", backend_name(&rt.name), spec.harness.as_str());
    let builder = format!("{} build", backend_name(&rt.name));
    let p = ui.proc(label.clone(), false);
    if let Some(c) = &daemon_client {
      c.proc_add(p.index(), &label, daemon::ProcKind::Build, None, Some(&builder), None, None, None, None, None);
    }
    harness_build_procs.push(p);
  }
  let mut skill_procs = Vec::with_capacity(skills.len());
  for skill in skills {
    let label = format!("{}: {}", skill.harness.as_str(), skill.name);
    let p = ui.proc(label.clone(), false);
    if let Some(c) = &daemon_client {
      c.proc_add(
        p.index(),
        &label,
        daemon::ProcKind::Skill,
        Some(skill.name.as_str()),
        Some(skill.harness.as_str()),
        skill.model.as_deref(),
        Some(skill.skill_source.as_str()),
        fleet_route_name(skill),
        None,
        None,
      );
    }
    if any_image_build {
      p.note("waiting for image build…");
    }
    skill_procs.push(p);
  }
  ui.pin_board_to_top();

  let mut build_failed = if let Some(ref base) = base_build {
    base.start();
    match run_build(
      base,
      &rt_name,
      runtime::BASE_IMAGE_TAG,
      runtime::BASE_IMAGE_TARGET,
      &df,
      uid,
      gid,
      &base_fp,
      false,
      daemon_client.as_deref(),
      "base",
      &session_id,
    ) {
      Ok(()) => {
        base.finish_ok(None);
        None
      }
      Err(e) => {
        base.finish_fail(failure::reason::BUILD_FAILED, Some(&e.0));
        Some(e)
      }
    }
  } else {
    None
  };

  if build_failed.is_none() {
    // Harness images depend only on the freshly built base, so they build in parallel
    // (one thread per image, same scoped-thread idiom as the skill runs below). All
    // builds run to completion; the first failure is the one reported.
    build_failed = std::thread::scope(|scope| {
      let session_ref = session_id.as_str();
      let handles: Vec<_> = harness_build_procs
        .iter()
        .zip(harness_builds.iter())
        .map(|(build, spec)| {
          let rt_name = &rt_name;
          let df = &df;
          let daemon_client = &daemon_client;
          scope.spawn(move || {
            build.start();
            match run_build(
              build,
              rt_name,
              &spec.tag,
              &spec.target,
              df,
              uid,
              gid,
              &spec.fingerprint,
              false,
              daemon_client.as_deref(),
              spec.harness.as_str(),
              session_ref,
            ) {
              Ok(()) => {
                build.finish_ok(None);
                None
              }
              Err(e) => {
                build.finish_fail(failure::reason::BUILD_FAILED, Some(&e.0));
                Some(e)
              }
            }
          })
        })
        .collect();
      handles
        .into_iter()
        .filter_map(|h| h.join().unwrap_or_else(|_| Some(("image build thread panicked".to_string(), 1))))
        .next()
    });
  }
  if let Some((msg, code)) = build_failed {
    ui.finish();
    fail(&msg);
    return code;
  }

  let caller_tip_ref = caller_tip.as_deref();
  for p in &skill_procs {
    p.note("starting…");
  }
  let run_started = std::time::Instant::now();
  let workload = stats::workload_of_repo(root);
  let outcomes: Vec<SkillRun> = std::thread::scope(|scope| {
    let dc = daemon_client.clone();
    let ui_ref = &ui;
    let session_ref = session_id.as_str();
    let handles: Vec<_> = skills
      .iter()
      .zip(skill_procs)
      .map(|(&skill, p)| {
        let dc = dc.clone();
        let first_index = p.index();
        scope.spawn(move || {
          let mut retry = RouteRetryState::new(skill.retry_for, skill.retry_signature_cap, session_ref, &skill.name);
          let mut proc = p;
          let mut proc_index = first_index;
          let mut attempts = 0u64;
          loop {
            attempts += 1;
            let attempt_started = std::time::Instant::now();
            let mut run =
              run_one_skill(skill, rt, root, secs, proc, caller_tip_ref, None, base, None, dc.clone(), session_ref);
            run.duration_secs = attempt_started.elapsed().as_secs_f64();
            run.proc_index = proc_index;
            run.attempts = attempts;
            if run.ok {
              // A browser restart that lost the race against this attempt's own finish
              // must not linger and respawn some future proc of the same index.
              daemon::consume_proc_restart(session_ref, proc_index);
              return run;
            }
            // A browser Force restart always respawns — each extra attempt costs the user
            // an explicit click, so no budget and no SCSH_NO_RETRY gate. Otherwise the
            // wall-clock retry contract: retryable failures (fresh clone, fresh container,
            // new live-board row) keep earning backed-off retries until the route's budget
            // runs out or the identical-failure breaker trips. A missing result from a TUI
            // harness also earns retries: its pane can be killed by a stray signal or
            // teardown before it writes the file, which a fresh run usually clears.
            retry.observe_failure(&run);
            let restart_requested = daemon::consume_proc_restart(session_ref, proc_index);
            let decision = retry_decision(
              run.fail_reason.as_deref(),
              restart_requested,
              false,
              retry.policy,
              retry.retries_used,
              retry.budget_spent_secs(),
              retry.consecutive_identical,
              failure::retry_enabled(),
              skill.harness.is_tui(),
              attempts == 1,
            );
            match decision {
              RetryDecision::Stop => return run,
              RetryDecision::StopBreaker => {
                retry.mark_breaker_tripped(&mut run);
                return run;
              }
              RetryDecision::Browser | RetryDecision::Schema | RetryDecision::Automatic => {}
            }
            let reason = if restart_requested {
              failure::reason::RESTART_REQUESTED
            } else {
              run.fail_reason.as_deref().unwrap_or("unknown")
            };
            failure::log_retry(session_ref, &skill.name, skill.harness.as_str(), skill.model.as_deref(), reason);
            let label = format!("{}: {} (retry)", skill.harness.as_str(), skill.name);
            let next = ui_ref.proc(label.clone(), false);
            if let Some(c) = &dc {
              c.proc_add(
                next.index(),
                &label,
                daemon::ProcKind::Skill,
                Some(skill.name.as_str()),
                Some(skill.harness.as_str()),
                skill.model.as_deref(),
                Some(skill.skill_source.as_str()),
                fleet_route_name(skill),
                None,
                Some(proc_index),
              );
            }
            proc_index = next.index();
            proc = next;
            if decision == RetryDecision::Automatic {
              let delay = retry.next_backoff_secs();
              proc.note(&format!(
                "retrying in ~{} after {reason} (retry {} of {}, {} and {} retries left)",
                ui::clock::format_elapsed(delay as f64),
                retry.retries_used,
                retry.policy.max_retries,
                ui::clock::format_elapsed(retry.budget_left_secs() as f64),
                retry.retries_left(),
              ));
              match backoff_sleep_interruptible(delay, session_ref, proc_index) {
                BackoffWake::Restart => proc.note("retrying now (browser restart)"),
                BackoffWake::Cancelled => {
                  proc.note("job stopped — not retrying");
                  proc.finish_fail(failure::reason::FORCE_STOPPED, Some("stopped from the session browser"));
                  run.fail_reason = Some(failure::reason::FORCE_STOPPED.into());
                  return run;
                }
                BackoffWake::Elapsed => {}
              }
            }
          }
        })
      })
      .collect();
    handles
      .into_iter()
      .zip(skills)
      .map(|(h, skill)| {
        h.join().unwrap_or_else(|_| {
          failure::log_skill(
            failure::reason::THREAD_PANICKED,
            &skill.name,
            "skill thread panicked before reporting outcome",
          );
          SkillRun::failed(failure::reason::THREAD_PANICKED, None, None, None)
        })
      })
      .collect()
  });

  // The run is over: restore the terminal and print the persistent ✓/✗ summary (attended; off a
  // TTY the per-proc lines already streamed). Everything below prints to the normal screen.
  ui.finish();

  // Fleet rollups: group multi-route skill_source results into deterministic JSON under the
  // session. Reconstruct minimal ProcRecords from skills + outcomes + on-disk result paths
  // (the daemon store is not readable from here).
  {
    use daemon::{ProcKind, ProcRecord, ProcStatus};
    let mut fake_procs = Vec::with_capacity(skills.len());
    for (skill, o) in skills.iter().zip(outcomes.iter()) {
      let safe = skill.name.replace('/', "_");
      let result_path = runtime::session_results_dir(&session_id).join(format!("{safe}.json"));
      let result_path = result_path.is_file().then(|| result_path.to_string_lossy().into_owned());
      fake_procs.push(ProcRecord {
        index: o.proc_index,
        previous_attempt: None,
        label: format!("{}: {}", skill.harness.as_str(), skill.name),
        kind: ProcKind::Skill,
        status: if o.graceful_shutdown {
          ProcStatus::Graceful
        } else if o.ok {
          ProcStatus::Ok
        } else {
          ProcStatus::Fail
        },
        skill_name: Some(skill.name.clone()),
        harness: Some(skill.harness.as_str().to_string()),
        model: skill.model.clone(),
        started_at: None,
        note: None,
        detail: o.result_content.as_deref().and_then(json::message),
        fail_reason: o.fail_reason.clone(),
        elapsed: Some(o.duration_secs),
        lines: vec![],
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: Some(skill.skill_source.clone()),
        route: fleet_route_name(skill).map(str::to_string),
        result_path,
        annotate_target: None,
      });
    }
    let _ = fleet::write_rollups(&session_id, &fake_procs);
  }

  // 3. The summary above carries each skill's ✓/✗ and detail; add run-dir/log pointers for any
  //    that failed, then the overall verdict.
  let n = outcomes.len();
  let failed = outcomes.iter().filter(|o| !o.ok).count();
  for (skill, o) in skills.iter().zip(outcomes.iter()).filter(|(_, o)| !o.ok) {
    if let Some(dir) = &o.run_dir {
      hint(&format!("run dir kept: {dir}"));
    }
    if let Some(log) = &o.log {
      hint(&format!("output log: {log}"));
    }
    let reason = o.fail_reason.as_deref().unwrap_or("unknown");
    let mut detail = String::new();
    if let Some(d) = &o.run_dir {
      detail.push_str(&format!("run dir: {d}\n"));
    }
    if let Some(l) = &o.log {
      detail.push_str(&format!("output log: {l}"));
    }
    failure::log_failed_skill(
      &session_id,
      &skill.name,
      skill.harness.as_str(),
      skill.model.as_deref(),
      reason,
      detail.trim(),
    );
  }
  if failed > 0 {
    failure::log_run_summary(&session_id, profile, failed, n);
    hint(&format!("failure log: {} (browse with `scsh failures`)", failure::log_path().display()));
  }

  // Persist run statistics (durable, ~/.scsh/stats.jsonl — browse with `scsh stats`): one
  // row per skill invocation with its route, outcome, duration, and the repo workload
  // (commits + LOC over main), plus one rollup row for the whole run.
  {
    let branch = current_branch(root);
    let repo = repo_path_for_session(root);
    for (skill, o) in skills.iter().zip(outcomes.iter()) {
      let outcome = if o.cached {
        "cached"
      } else if o.ok {
        "ok"
      } else {
        "fail"
      };
      stats::record(&stats::StatRecord {
        ts: secs,
        kind: "skill".into(),
        session: session_id.clone(),
        repo: repo.clone(),
        branch: branch.clone(),
        profile: profile.map(str::to_string),
        skill: Some(skill.name.clone()),
        skill_source: Some(skill.skill_source.clone()),
        harness: Some(skill.harness.as_str().to_string()),
        model: skill.model.clone(),
        effort: skill.effort.clone(),
        outcome: Some(outcome.into()),
        fail_reason: o.fail_reason.clone(),
        attempts: o.attempts,
        duration_secs: o.duration_secs,
        commits: workload.commits,
        loc_added: workload.loc_added,
        loc_deleted: workload.loc_deleted,
        skills_total: None,
        skills_failed: None,
      });
    }
    stats::record(&stats::StatRecord {
      ts: secs,
      kind: "run".into(),
      session: session_id.clone(),
      repo,
      branch,
      profile: profile.map(str::to_string),
      attempts: 1,
      duration_secs: run_started.elapsed().as_secs_f64(),
      commits: workload.commits,
      loc_added: workload.loc_added,
      loc_deleted: workload.loc_deleted,
      skills_total: Some(n as u64),
      skills_failed: Some(failed as u64),
      ..Default::default()
    });
  }

  // 4. Pull commits OUT from commit-enabled skills (host-only, after containers exit).
  //    Runs SEQUENTIALLY: each skill's new commits in its run clone (base..clone-HEAD)
  //    are fetched from the LOCAL clone path — not from GitHub — and cherry-picked onto
  //    the caller's branch. Only when commits: true AND the skill actually committed.
  //    Commits that don't apply cleanly are saved to scsh/incoming/<skill>-… instead.
  if let Some(caller_tip) = &caller_tip {
    let stamp = runtime::format_utc_timestamp(secs);
    for (skill, o) in skills.iter().zip(outcomes.iter()) {
      if !skill.commits {
        continue;
      }
      // A live clone integrates its commits directly; a commit-enabled cache HIT replays
      // the commits journaled in the cache, so a hit reproduces the commit, not just the result.
      let integration = if let Some(clone) = &o.clone_dir {
        integrate_commits(root, clone, caller_tip, &skill.name, &stamp)
      } else if let Some(patch) = &o.cached_commits {
        apply_cached_commits(root, patch, &skill.name, &stamp)
      } else {
        continue;
      };
      match integration {
        Ok(None) => {}
        Ok(Some(Integration::Applied { count, range })) => {
          ok(&format!(
            "{}: brought in {count} commit{} (rebased onto {})",
            skill.name,
            plural(count),
            current_branch(root)
          ));
          pack_step_diff(root, &session_id, skill, o, range, daemon_session.client.as_deref());
        }
        Ok(Some(Integration::Saved { branch, count, range })) => {
          warn(&format!(
            "{}: {count} commit{} didn't rebase cleanly — saved to branch {branch} (inspect, then merge/cherry-pick)",
            skill.name,
            plural(count)
          ));
          pack_step_diff(root, &session_id, skill, o, range, daemon_session.client.as_deref());
        }
        Err(e) => warn(&format!("{}: could not bring in commits — {e}", skill.name)),
      }
    }
    if let Some(head) = git_capture(root, &["rev-parse", "HEAD"]) {
      pack_job_diff(root, &session_id, caller_tip, head.trim());
    }
  }

  // 5. Tidy up. A successful skill's clone has served its purpose — the result was
  //    collected and any commits integrated — so remove it (the container was already
  //    `--rm`; this is the host-side scratch). A FAILED skill's clone is kept for
  //    inspection (its path was printed above). Opt out entirely with SCSH_KEEP_RUNS=1.
  if !keep_run_dirs() {
    for o in outcomes.iter().filter(|o| o.ok) {
      if let Some(clone) = &o.clone_dir {
        let _ = std::fs::remove_dir_all(clone);
      }
    }
  }

  // Repeat the session deep link as one of the LAST lines: when a coding agent drives
  // `scsh run`, the tail of the output is what it relays to the human — the clickable
  // link to this run's recordings must be there, not only in the opening lines.
  if let Some(c) = &daemon_session.client {
    ok(&format!("session recordings & live board: {}", c.session_url()));
  }

  // Annotate while the client is still registered (before DaemonSession drop / finish_session).
  let mut next_annotate_idx = outcomes.iter().map(|o| o.proc_index).max().map(|m| m + 1).unwrap_or(0);
  annotate_run_casts(root, session_skill_casts(&session_id), daemon_session.client.as_deref(), &mut next_annotate_idx);

  if failed == 0 {
    ok(&format!("all {n} skill{} completed successfully", plural(n)));
    0
  } else {
    fail(&format!("{failed} of {n} skill{} failed", plural(n)));
    1
  }
}

/// The outcome of running one skill end to end (clone → harness → collect). The per-skill ✓/✗
/// and its detail are shown by the live board (and its final summary); this is the structured
/// residue the orchestrator still needs afterward — run-dir/log pointers and commit replay.
struct SkillRun {
  ok: bool,
  /// The live-board row this outcome belongs to (the retry's row when a retry ran) — set
  /// by the orchestrator, so post-run residue (the packed commits diff) lands on the row
  /// that actually produced it.
  proc_index: usize,
  /// Stable reason code when `ok == false`.
  fail_reason: Option<String>,
  /// Human diagnostic associated with `fail_reason`, retained so the workflow orchestrator can
  /// explain and link its one fresh result-schema repair attempt.
  fail_detail: Option<String>,
  /// Served from the content-addressed cache (no clone, no container).
  cached: bool,
  /// Wall-clock seconds of the (final) attempt — set by the orchestrator, for stats.
  duration_secs: f64,
  /// How many times this route ran: 1, plus one for the automatic transient-failure
  /// retry, plus one per browser Force restart — set by the orchestrator.
  attempts: u64,
  /// The `/tmp` run dir, kept for inspection when the skill failed.
  run_dir: Option<String>,
  /// Host path to the skill's output log, when its container actually ran.
  log: Option<String>,
  /// The skill's clone, set whenever the clone succeeded (whatever the outcome), so
  /// a commit-enabled skill's commits can be brought back afterward. `None` if no
  /// clone was made (e.g. a refused or pre-clone failure).
  clone_dir: Option<PathBuf>,
  /// For a commit-enabled skill served from cache: the journaled commits as a git
  /// `format-patch` mbox, replayed onto the caller's branch so a hit reproduces the
  /// commit side effect (not just the result file). `None` otherwise.
  cached_commits: Option<String>,
  /// The skill's result-file JSON as read from the host after the run (or the cache), so a
  /// workflow orchestrator can feed one step's output into the next. `None` when no result
  /// was produced (a failure) or it could not be read.
  result_content: Option<String>,
  /// Typed workflow fields validated before this run's proc became terminal. `None` for flat
  /// skills, which have no workflow result contract.
  workflow_outputs: Option<std::collections::HashMap<String, String>>,
  /// The durable task result passed, but the harness or container did not finish cleanly. This
  /// remains an `ok` outcome while rendering orange so teardown trouble stays visible.
  graceful_shutdown: bool,
}

impl SkillRun {
  fn base() -> SkillRun {
    SkillRun {
      ok: false,
      proc_index: 0,
      fail_reason: None,
      fail_detail: None,
      cached: false,
      duration_secs: 0.0,
      attempts: 1,
      run_dir: None,
      log: None,
      clone_dir: None,
      cached_commits: None,
      result_content: None,
      workflow_outputs: None,
      graceful_shutdown: false,
    }
  }
  fn ok(
    log: String, clone_dir: Option<PathBuf>, result_content: Option<String>,
    workflow_outputs: Option<std::collections::HashMap<String, String>>,
  ) -> SkillRun {
    SkillRun { ok: true, log: Some(log), clone_dir, result_content, workflow_outputs, ..SkillRun::base() }
  }
  fn graceful(
    log: String, clone_dir: Option<PathBuf>, result_content: Option<String>,
    workflow_outputs: Option<std::collections::HashMap<String, String>>,
  ) -> SkillRun {
    SkillRun {
      ok: true,
      graceful_shutdown: true,
      log: Some(log),
      clone_dir,
      result_content,
      workflow_outputs,
      ..SkillRun::base()
    }
  }
  fn failed(reason: &str, run_dir: Option<String>, log: Option<String>, clone_dir: Option<PathBuf>) -> SkillRun {
    SkillRun { fail_reason: Some(reason.into()), run_dir, log, clone_dir, ..SkillRun::base() }
  }
  /// Attach the human "why" to a failed run — the first line feeds the retry loops'
  /// identical-failure signatures, so two attempts failing the same way are recognizable.
  fn with_fail_detail(mut self, why: &str) -> SkillRun {
    self.fail_detail = Some(why.to_string());
    self
  }
  fn invalid_result(
    detail: String, run_dir: String, log: String, clone_dir: Option<PathBuf>, result_content: String,
  ) -> SkillRun {
    SkillRun {
      fail_reason: Some(failure::reason::RESULT_INVALID.into()),
      fail_detail: Some(detail),
      run_dir: Some(run_dir),
      log: Some(log),
      clone_dir,
      result_content: Some(result_content),
      ..SkillRun::base()
    }
  }
  /// A cache hit: the result was restored from the cache without running the skill (no
  /// clone, no container). `cached_commits` carries any journaled commits to replay, so a
  /// hit for a commit-enabled skill still reproduces the commit. `result_content` is the
  /// restored JSON — workflows need it to bind downstream `inputs:` / `when:` from this
  /// step's outputs (a hit that only wrote the file to disk left the DAG with nothing to read).
  fn cached(
    cached_commits: Option<String>, result_content: String,
    workflow_outputs: Option<std::collections::HashMap<String, String>>,
  ) -> SkillRun {
    SkillRun {
      ok: true,
      cached: true,
      cached_commits,
      result_content: Some(result_content),
      workflow_outputs,
      ..SkillRun::base()
    }
  }
}

/// One detail string for a failed skill — shown in the terminal summary and session browser.
fn skill_fail_detail(why: &str, harness: config::Harness, run_dir: Option<&str>, log: Option<&str>) -> String {
  let mut parts = vec![why.to_string()];
  if let Some(d) = run_dir {
    parts.push(format!("run dir: {d}"));
  }
  if let Some(l) = log {
    parts.push(format!("output log: {l}"));
    if runtime::harness_verbose_enabled() {
      match harness {
        config::Harness::Claude => parts.push(format!("claude debug log: {l}.debug")),
        config::Harness::Codex => parts.push(format!("codex final message: {l}.last")),
        config::Harness::Grok => parts.push(format!("grok debug log: {l}.debug")),
        config::Harness::Cursor => {}
        config::Harness::Opencode => {}
      }
    }
  }
  parts.join("\n")
}

/// The rendered text of a recording's final stretch, for failure classification: parse the
/// cast's trailing output events, strip terminal escapes, and keep roughly the last
/// screenful. A TUI's telling last words — "Reconnecting to …", a 529 page, a login
/// demand — live only here; the proc lines carry the wrapper's own output. Best-effort:
/// a missing or unparsable cast yields an empty sample.
fn cast_tail_text(cast: &Path) -> String {
  const READ_BYTES: u64 = 32 * 1024;
  const KEEP_CHARS: usize = 4 * 1024;
  use std::io::{Read, Seek, SeekFrom};
  let Ok(mut file) = std::fs::File::open(cast) else { return String::new() };
  let len = file.metadata().map(|m| m.len()).unwrap_or(0);
  let start = len.saturating_sub(READ_BYTES);
  if file.seek(SeekFrom::Start(start)).is_err() {
    return String::new();
  }
  let mut raw = Vec::new();
  if file.read_to_end(&mut raw).is_err() {
    return String::new();
  }
  let raw = String::from_utf8_lossy(&raw);
  let mut text = String::new();
  // Reading from mid-file, the first line is usually truncated — skip it.
  for line in raw.lines().skip(if start > 0 { 1 } else { 0 }) {
    let Ok(json::Value::Array(event)) = json::parse(line) else { continue };
    if let (Some(json::Value::String(kind)), Some(json::Value::String(data))) = (event.get(1), event.get(2)) {
      if kind == "o" {
        text.push_str(&console::strip_ansi_codes(data));
      }
    }
  }
  if text.len() > KEEP_CHARS {
    let mut cut = text.len() - KEEP_CHARS;
    while !text.is_char_boundary(cut) {
      cut += 1;
    }
    text.split_off(cut)
  } else {
    text
  }
}

fn apple_container_lost_shell_response(last: Option<&str>) -> bool {
  last.is_some_and(|line| {
    line.contains("failed to send signal") && line.contains("missing signal in xpc message") && line.contains("signal")
  })
}

fn inner_harness_result_is_good(run_dir: &Path, result_rel: &str, commits: bool) -> bool {
  let exit = run_dir.join(format!("{}.exit", runtime::RUN_LOG_REL));
  let exit_zero = std::fs::read_to_string(exit).is_ok_and(|code| code.trim() == "0");
  if !exit_zero {
    return false;
  }
  let result_good = std::fs::read_to_string(run_dir.join(result_rel))
    .ok()
    .and_then(|body| json::parse(&body).ok())
    .is_some_and(|value| matches!(value, json::Value::Object(_)));
  if !result_good || !commits {
    return result_good;
  }
  git_capture(&run_dir.join(runtime::PULL_BARE), &["for-each-ref", "--format=%(refname)", "refs/heads"])
    .is_some_and(|refs| !refs.trim().is_empty())
}

/// Whether an interrupted or non-cleanly-exited harness crossed its durable result boundary first.
///
/// This intentionally asks only "is the declared file
/// present?" It does *not* pronounce the task successful. The common result path below still
/// copies the file, parses the workflow schema when one exists, rejects missing/extra/wrongly
/// typed fields, and performs the bounded schema-correction retry. Keeping those checks in one
/// place is important: an unreliable TUI exit must not bypass the task's data contract, while a
/// reliable data contract must not be discarded merely because the TUI failed to close after
/// writing it.
///
/// The ordering is safe because the harness exited or a watchdog already stopped its container
/// when this is called. The file is therefore no longer being written. A partial file can be present,
/// but it will fail the same downstream parsing/schema validation as any other malformed result.
fn interrupted_harness_result_is_recoverable(run_dir: &Path, result_rel: &str) -> bool {
  run_dir.join(result_rel).is_file()
}

/// Apple Container can lose the outer `container run` response while leaving the container
/// alive. The harness remains authoritative: wait only for the same bounded inactivity window
/// it normally receives, and accept recovery only when both its result JSON and inner exit-0
/// marker arrive.
fn wait_for_inner_harness_result(run_dir: &Path, result_rel: &str, commits: bool, limit: Duration) -> bool {
  let deadline = Instant::now() + limit;
  loop {
    if inner_harness_result_is_good(run_dir, result_rel, commits) {
      return true;
    }
    if Instant::now() >= deadline {
      return false;
    }
    std::thread::sleep(Duration::from_millis(200));
  }
}

/// Run a single skill end to end in its own clone and container, driving `spinner`
/// through its phases and finishing it ✓/✗. Returns the structured outcome.
#[allow(clippy::too_many_arguments)]
fn run_one_skill(
  skill: &ResolvedInvocation, rt: &Runtime, root: &Path, secs: u64, spinner: ui::screen::Proc,
  caller_tip: Option<&str>, source_revision: Option<&str>, base: Option<&RunBase>,
  result_contract: Option<WorkflowResultContract<'_>>, daemon_client: Option<std::sync::Arc<daemon::Client>>,
  session_id: &str,
) -> SkillRun {
  // Mark the row running so its clock starts and output stamps are relative to here.
  spinner.start();
  // Wall clock for this run. On success its duration is stored with the cached artifact as
  // provenance, but a future cache-hit attempt keeps its own (near-zero) elapsed clock.
  let run_started = Instant::now();
  // Resolve forwarded env first: a missing required (${VAR:?…}) variable refuses
  // the skill before any work — no clone, no container.
  let env = match resolve_env(&skill.env) {
    Ok(mut e) => {
      e.push(("SCSH_RESULT".to_string(), skill.result.clone()));
      e
    }
    Err(message) => {
      spinner.finish_fail(failure::reason::ENV_UNRESOLVED, Some(&message));
      return SkillRun::failed(failure::reason::ENV_UNRESOLVED, None, None, None);
    }
  };

  // Content-addressed cache: if this exact repo content + skill + env was run before,
  // restore the cached result and finish — no clone, no container, no commit. (The key
  // is computed from the caller's committed state, which is what the clone would be.)
  let key = cache_key_at(root, skill, &env, source_revision, base);
  if let Some(key) = &key {
    if let Some(entry) = cache_lookup(root, key) {
      let workflow_outputs = result_contract.and_then(|contract| extract_step_outputs(&entry.result, contract).ok());
      let valid = result_contract.is_none() || workflow_outputs.is_some();
      if valid && restore_cached_result(root, &skill.result, &entry.result).is_ok() {
        let provenance = cache_hit_provenance(entry.cached_at, entry.elapsed);
        let message = json::message(&entry.result)
          .or_else(|| result_contract.zip(workflow_outputs.as_ref()).and_then(|(c, o)| workflow_outputs_glimpse(c, o)));
        let line = match message {
          Some(m) => format!("{}  ({provenance})", first_line(&m)),
          None => format!("({provenance})"),
        };
        // Replay the original recording as provenance. Its duration belongs to the source run
        // and is labeled in `line`; this cache-hit attempt's own elapsed clock stays near zero.
        if let (Some(c), Some(cast)) = (&daemon_client, &entry.cast) {
          // Restore chapters next to the cached cast so the session browser finds them the
          // same way it finds a live run's sidecar (`<stem>.chapters.json`).
          if let Some(src) = &entry.chapters {
            if let Some(dest) = daemon::chapters_sidecar_path(&cast.to_string_lossy()) {
              let _ = std::fs::copy(src, dest);
            }
          }
          c.proc_cast(spinner.index(), &cast.to_string_lossy());
        }
        // Durable result copy for fleet rollups / job-page comparison (same path as a live run).
        let host_result = root.join(&skill.result);
        if let Some(path) = fleet::persist_skill_result(session_id, &skill.name, &host_result) {
          if let Some(c) = &daemon_client {
            c.proc_result(spinner.index(), &path);
          }
        }
        spinner.finish_ok(Some(&line));
        // Carry any journaled commits so they're replayed onto the caller's branch — a hit
        // for a commit-enabled skill reproduces the commit, not just the result file.
        // Also carry the result JSON so a workflow can feed this step's outputs into later
        // steps (without it, run_workflow aborts with "produced no result" after a cache hit).
        return SkillRun::cached(entry.commits, entry.result, workflow_outputs);
      }
    }
  }

  // Own run dir on the HOST (push IN). Either a full clone bind-mounted into the container,
  // or (macOS Apple Container) a bare transport repo + git daemon the container clones from.
  // After the container exits, scsh pulls the result file OUT; commits too when commits: true.
  spinner.note("preparing repo…");
  let run_dir = match prepare_run_dir(secs, &skill.name, &rt.name) {
    Ok(d) => d,
    Err(e) => {
      spinner.finish_fail(failure::reason::RUN_DIR, Some(&e));
      return SkillRun::failed(failure::reason::RUN_DIR, None, None, None);
    }
  };
  let run_dir_str = run_dir.to_string_lossy().into_owned();
  let git_transport = runtime::uses_git_transport(&rt.name);
  let mut git_daemon = None;
  if git_transport {
    if let Err(e) = prepare_git_transport(root, &run_dir, skill.commits, source_revision, base, &spinner) {
      spinner.finish_fail(failure::reason::GIT_TRANSPORT, Some(&e));
      return SkillRun::failed(failure::reason::GIT_TRANSPORT, Some(run_dir_str), None, None);
    }
    match GitTransport::start(&run_dir) {
      Ok(d) => git_daemon = Some(d),
      Err(e) => {
        spinner.finish_fail(failure::reason::GIT_DAEMON, Some(&e));
        return SkillRun::failed(failure::reason::GIT_DAEMON, Some(run_dir_str), None, None);
      }
    }
  } else if let Err(e) = clone_into(root, &run_dir, source_revision, skill.commit_identity.as_ref(), base, &spinner) {
    spinner.finish_fail(failure::reason::CLONE, Some(&e));
    return SkillRun::failed(failure::reason::CLONE, Some(run_dir_str), None, None);
  }
  // From here the clone exists — carry it so a commit-enabled skill's commits can be
  // brought back even if a later step fails.
  let clone_dir = Some(run_dir.clone());

  // A harness-definition run (`scsh run --def`) carries its SKILL.md body; write it into the
  // clone so the agent finds it at `.skills/<name>/SKILL.md`. The caller's working tree stays
  // clean. A normal `.scsh.yml` skill has no body and this is a no-op.
  if let Err(e) = materialize_skill_body(&run_dir, git_transport, skill) {
    spinner.finish_fail(failure::reason::CLONE, Some(&e));
    return SkillRun::failed(failure::reason::CLONE, Some(run_dir_str), None, clone_dir);
  }

  // Ensure the result's parent dir exists in the clone so the skill can write it
  // even if the harness's tool does not `mkdir -p`.
  if let Some(parent) = Path::new(&skill.result).parent() {
    if !parent.as_os_str().is_empty() {
      let _ = std::fs::create_dir_all(run_dir.join(parent));
    }
  }

  // Run the harness command in a named container with the clone mounted at /home/agent/repo,
  // under the skill's optional wall-clock timeout.
  spinner.note(&format!("{} run…", skill.harness.as_str()));
  let name = run_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| skill.name.clone());
  // The harness tees its output to this log under the mount's gitignored tmp/ (= on the host).
  // Create its parent so `tee` can write even before the skill touches tmp/.
  let log_path = run_dir.join(runtime::RUN_LOG_REL);
  if let Some(parent) = log_path.parent() {
    let _ = std::fs::create_dir_all(parent);
  }
  let log = log_path.to_string_lossy().into_owned();
  // Copy host opencode auth/config into the run clone's tmp/ (rides the repo mount; no mounts).
  let opencode_forward = if skill.harness == config::Harness::Opencode && opencode_auth_enabled() {
    forward_opencode(&run_dir)
  } else {
    None
  };
  let claude_auth = if skill.harness == config::Harness::Claude && claude_auth_enabled() {
    forward_claude_auth(&run_dir)
  } else {
    None
  };
  let codex_auth =
    if skill.harness == config::Harness::Codex && codex_auth_enabled() { forward_codex(&run_dir) } else { None };
  let grok_auth =
    if skill.harness == config::Harness::Grok && grok_auth_enabled() { forward_grok(&run_dir) } else { None };
  let cursor_auth =
    if skill.harness == config::Harness::Cursor && cursor_auth_enabled() { forward_cursor(&run_dir) } else { false };
  let tag = runtime::image_tag(skill.harness);
  // Claude needs no extra mounts: its forwarded config lives under the run clone's
  // tmp/.claude-auth (the image's CLAUDE_CONFIG_DIR), riding along with the repo mount —
  // and stays WRITABLE, which the interactive TUI requires (single-file bind mounts are
  // read-only under Apple containers, and an unwritable config re-triggers onboarding).
  let mut vols: Vec<(String, String)> = runtime::harness_volumes(skill.harness);
  // Git transport bind-mounts back only `tmp/`. When this skill writes its result under the
  // alternate `.harness/tmp` scratch, mount that too so the result round-trips to the host.
  if git_transport && skill.result.starts_with(".harness/tmp/") {
    let host = run_dir.join(".harness").join("tmp");
    let _ = std::fs::create_dir_all(&host);
    vols.push((host.to_string_lossy().into_owned(), format!("{}/.harness/tmp", runtime::AGENT_REPO)));
  }
  let vol_refs: Vec<(&str, &str)> = vols.iter().map(|(h, m)| (h.as_str(), m.as_str())).collect();
  let mut container_env = env.clone();
  if skill.harness == config::Harness::Claude {
    if let Some(token) = runtime::claude_oauth_token() {
      container_env.push((runtime::CLAUDE_OAUTH_TOKEN_ENV.to_string(), token));
    }
  }
  if skill.harness == config::Harness::Codex {
    if let Ok(key) = std::env::var(runtime::OPENAI_API_KEY_ENV) {
      if !key.is_empty() {
        container_env.push((runtime::OPENAI_API_KEY_ENV.to_string(), key));
      }
    }
  }
  if skill.harness == config::Harness::Grok {
    if let Ok(key) = std::env::var(runtime::XAI_API_KEY_ENV) {
      if !key.is_empty() {
        container_env.push((runtime::XAI_API_KEY_ENV.to_string(), key));
      }
    }
  }
  if skill.harness == config::Harness::Cursor {
    if let Some(key) = runtime::cursor_api_key() {
      container_env.push((runtime::CURSOR_API_KEY_ENV.to_string(), key));
    }
  }
  container_env.extend(runtime::harness_container_env(skill.harness));
  if let Some(d) = &git_daemon {
    container_env.extend(d.env());
  }
  let harness = runtime::harness_command(
    skill.harness,
    skill.model.as_deref(),
    skill.effort.as_deref(),
    &skill.skill_source,
    &skill.result,
    skill.terminal,
    &skill.delivery,
  );
  let cmd = if git_transport {
    let (ci_name, ci_email) = skill
      .commit_identity
      .as_ref()
      .map(|(n, e)| (n.as_str(), e.as_str()))
      .unwrap_or((SCSH_COMMIT_NAME, SCSH_COMMIT_EMAIL));
    runtime::git_transport_entry(&harness, skill.commits, ci_name, ci_email)
  } else {
    harness
  };
  let repo_mount = if git_transport { runtime::RepoMountMode::TmpOnly } else { runtime::RepoMountMode::Full };
  let run = runtime::run_command(&rt.name, &tag, &run_dir_str, &name, &container_env, &vol_refs, &cmd, repo_mount);
  let timeout = skill.timeout.map(Duration::from_secs);
  // Screen-inactivity watchdog: the bind-mounted cast is the heartbeat, counting only NOVEL
  // frames (timestamps stripped, digits erased) — so both a frozen TUI and a wedged one
  // hiding behind a looping spinner (hung login, exhausted quota, gateway retry loop) are
  // killed rather than waiting out the full wall-clock timeout.
  let inactivity_secs = config::effective_inactivity_timeout(skill.harness, skill.inactivity_timeout);
  let watch = ui::screen::ActivityWatch {
    file: run_dir.join(runtime::RUN_CAST_REL),
    limit: Duration::from_secs(inactivity_secs),
  };
  let _container = ui::signals::ContainerGuard::new(&rt.name, &name);
  if let Some(c) = &daemon_client {
    c.container_event(spinner.index(), "start", &name, &rt.name);
    // The bind-mounted cast grows on the host while the harness runs; registering it now
    // lets the session browser download/replay the recording mid-run.
    c.proc_cast(spinner.index(), &run_dir.join(runtime::RUN_CAST_REL).to_string_lossy());
  }
  let result = spinner.run_watched(&run[0], &run[1..], timeout, Some(&watch));
  // A terminal harness process and a completed task are related, but they are not identical.
  // The declared result file is the durable task boundary. Some interactive CLIs finish the
  // requested work, write that result, and then wedge or return a misleading status while their
  // TUI is closing. In those cases the result still goes through the normal validation path and,
  // if valid, the proc is successful-but-orange (`graceful`) rather than failed-red. Retain the
  // infrastructure wrinkle as a reason string so the UI remains honest about what happened.
  let mut graceful_shutdown_reason: Option<&'static str> = None;
  let result = match result {
    Ok((false, ui::screen::Killed::No, last))
      if rt.name == "container" && apple_container_lost_shell_response(last.as_deref()) =>
    {
      let recovery_limit = timeout
        .map(|limit| limit.saturating_sub(run_started.elapsed()))
        .unwrap_or_else(|| Duration::from_secs(inactivity_secs));
      if wait_for_inner_harness_result(&run_dir, &skill.result, skill.commits, recovery_limit) {
        graceful_shutdown_reason =
          Some("accepted the valid result and inner exit 0 after Apple Container lost its shell response");
        Ok((true, ui::screen::Killed::No, last))
      } else {
        Ok((false, ui::screen::Killed::No, last))
      }
    }
    Ok((false, killed, last)) if interrupted_harness_result_is_recoverable(&run_dir, &skill.result) => {
      // The durable result is the task boundary. A timeout, inactivity kill, or later non-zero
      // TUI exit does not erase work already written there; collection and schema validation below
      // remain authoritative, and only a result that survives them becomes graceful success.
      graceful_shutdown_reason = Some(match killed {
        ui::screen::Killed::Inactive => "accepted the valid result after the inactivity watchdog stopped the harness",
        ui::screen::Killed::Timeout => "accepted the valid result after the wall-clock watchdog stopped the harness",
        ui::screen::Killed::No => "accepted the valid result despite the harness exiting non-zero during teardown",
      });
      Ok((true, killed, last))
    }
    other => other,
  };
  // `--rm` is the normal path, but verify cleanup eagerly. A killed runtime client can leave a
  // live container behind, and Apple Container can retain stopped containers and their disk.
  // `stop_container` returns immediately when the named container is already gone.
  ui::signals::stop_container(&rt.name, &name);
  if let Some(c) = &daemon_client {
    c.container_event(spinner.index(), "stop", &name, &rt.name);
  }
  // Run dirs are pruned shortly after the skill ends (on any outcome); keep the recording
  // and logs under $SCSH_HOME (default ~/.scsh) so session export survives throwaway clones.
  let durable_cast = persist_run_artifacts(session_id, &run_dir, &skill.name, secs);
  if let (Some(c), Some(durable)) = (&daemon_client, &durable_cast) {
    c.proc_cast(spinner.index(), durable);
  }
  if let Some(p) = &claude_auth {
    let _ = std::fs::remove_dir_all(p);
  }
  if let Some(p) = &opencode_forward {
    let _ = std::fs::remove_dir_all(p);
  }
  if let Some(p) = &codex_auth {
    scrub_codex_credentials(p);
  }
  if let Some(p) = &grok_auth {
    scrub_grok_credentials(p);
  }
  if cursor_auth {
    scrub_cursor_credentials(&run_dir);
  }
  match result {
    Ok((true, _, _)) => {
      if let Some(durable) = durable_cast.as_deref() {
        spawn_cast_annotation(Path::new(durable));
      }
    }
    Ok((false, ui::screen::Killed::Timeout, _)) => {
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let why = format!("timed out after {}s", skill.timeout.unwrap_or(0));
      let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::CONTAINER_TIMEOUT, Some(&detail));
      return SkillRun::failed(failure::reason::CONTAINER_TIMEOUT, Some(run_dir_str), Some(log), clone_dir)
        .with_fail_detail(&why);
    }
    Ok((false, ui::screen::Killed::Inactive, _)) => {
      // The recorded screen froze past the watchdog limit. Cleanup already ran above; retain
      // its own reason so stats can tell a stuck harness from a slow one.
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let why = format!("no new screen content for {inactivity_secs}s (inactivity_timeout)");
      let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::CONTAINER_INACTIVE, Some(&detail));
      return SkillRun::failed(failure::reason::CONTAINER_INACTIVE, Some(run_dir_str), Some(log), clone_dir)
        .with_fail_detail(&why);
    }
    Ok((false, ui::screen::Killed::No, last)) => {
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let tail = spinner.tail_lines(failure::FAILURE_TAIL_LINES);
      let why = failure::failure_excerpt(last.as_deref(), &tail, "harness exited non-zero (no output captured)");
      // Classify from the excerpt AND the rendered cast tail: the proc lines carry only the
      // wrapper's own output for a TUI harness, while the screen that matters — cursor's
      // "Reconnecting to …", a provider's 529 page, a login demand — lives in the recording.
      let sample = format!("{why}\n{}", cast_tail_text(&run_dir.join(runtime::RUN_CAST_REL)));
      let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
      let reason = if failure::harness_reported_overload(&sample) {
        failure::reason::HARNESS_OVERLOADED
      } else if failure::harness_reported_disconnect(&sample) {
        failure::reason::HARNESS_DISCONNECTED
      } else if failure::harness_reported_auth_rejection(&sample) {
        failure::reason::HARNESS_AUTH_REJECTED
      } else {
        failure::reason::HARNESS_NONZERO
      };
      spinner.finish_fail(reason, Some(&detail));
      return SkillRun::failed(reason, Some(run_dir_str), Some(log), clone_dir).with_fail_detail(&why);
    }
    Err(e) => {
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let why = format!("could not run container: {e}");
      let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::CONTAINER_RUN, Some(&detail));
      return SkillRun::failed(failure::reason::CONTAINER_RUN, Some(run_dir_str), Some(log), clone_dir)
        .with_fail_detail(&why);
    }
  }

  // Pull the result file OUT of the run clone into the caller repo (host-side, always).
  // The result file is required: missing → this skill (and the whole run) fails. Declared
  // artifacts are part of the same contract: each is copied back beside the result, and a
  // missing one fails the skill exactly like a missing result.
  for artifact in &skill.artifacts {
    if let Err(e) = collect_skill_result(root, &run_dir, artifact, secs) {
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let why = format!("declared artifact: {e}");
      let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::RESULT_MISSING, Some(&detail));
      return SkillRun::failed(failure::reason::RESULT_MISSING, Some(run_dir_str), Some(log), clone_dir)
        .with_fail_detail(&why);
    }
  }
  match collect_skill_result(root, &run_dir, &skill.result, secs) {
    Ok(dest) => {
      // Cache the result content under this run's key, so an identical future run
      // (same repo content + skill + env) is a hit. Then show the skill's *message*,
      // not just the file (its `result`/`message`/sole field — see json::message),
      // falling back for workflow steps to a glimpse of their declared scalar outputs,
      // and only then to the result path; a multi-line message shows its first line.
      let content = match std::fs::read_to_string(&dest) {
        Ok(content) => content,
        Err(error) => {
          schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
          let why = format!("could not read collected result '{}': {error}", skill.result);
          let detail = skill_fail_detail(&why, skill.harness, Some(&run_dir_str), Some(&log));
          spinner.finish_fail(failure::reason::RESULT_MISSING, Some(&detail));
          return SkillRun::failed(failure::reason::RESULT_MISSING, Some(run_dir_str), Some(log), clone_dir)
            .with_fail_detail(&why);
        }
      };
      let workflow_outputs = match result_contract.map(|contract| extract_step_outputs(&content, contract)) {
        Some(Err(error)) => {
          if let Some(path) = fleet::persist_skill_result(session_id, &skill.name, Path::new(&dest)) {
            if let Some(c) = &daemon_client {
              c.proc_result(spinner.index(), &path);
            }
          }
          // The workflow owns one bounded correction attempt. Return the validation error so its
          // orchestrator can settle this attempt and register a fresh, explicitly linked proc.
          spinner.note("result schema invalid; preparing one correction retry…");
          return SkillRun::invalid_result(error, run_dir_str, log, clone_dir, content);
        }
        Some(Ok(outputs)) => Some(outputs),
        None => None,
      };
      if let Some(key) = &key {
        // Journal a commit-enabled skill's new commits (base..clone-HEAD) as a patch
        // alongside the result, so a future cache hit can replay them.
        let commits = if skill.commits {
          caller_tip.and_then(|b| commit_patch(&runtime::commits_fetch_path(&run_dir), b))
        } else {
          None
        };
        cache_store(
          root,
          key,
          &content,
          commits.as_deref(),
          Some(run_started.elapsed().as_secs_f64()),
          durable_cast.as_deref().map(Path::new),
        );
      }
      let message = json::message(&content)
        .or_else(|| result_contract.zip(workflow_outputs.as_ref()).and_then(|(c, o)| workflow_outputs_glimpse(c, o)));
      let headline = message.as_deref().map(first_line).unwrap_or(skill.result.as_str());
      // Register the durable result before publishing the terminal proc transition. A browser
      // that observes green must already be able to read the exact result that earned it.
      if let Some(path) = fleet::persist_skill_result(session_id, &skill.name, Path::new(&dest)) {
        if let Some(c) = &daemon_client {
          c.proc_result(spinner.index(), &path);
        }
      }
      if let Some(reason) = graceful_shutdown_reason {
        let detail = format!("{headline}\n\nGraceful shutdown: {reason}.");
        spinner.finish_graceful(Some(&detail));
      } else {
        spinner.finish_ok(Some(headline));
      }
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, true);
      if graceful_shutdown_reason.is_some() {
        SkillRun::graceful(log, clone_dir, Some(content), workflow_outputs)
      } else {
        SkillRun::ok(log, clone_dir, Some(content), workflow_outputs)
      }
    }
    Err(e) => {
      schedule_run_dir_prune_backup(daemon_client.as_ref(), &run_dir_str, &name, &rt.name, false);
      let detail = skill_fail_detail(&e, skill.harness, Some(&run_dir_str), Some(&log));
      spinner.finish_fail(failure::reason::RESULT_MISSING, Some(&detail));
      SkillRun::failed(failure::reason::RESULT_MISSING, Some(run_dir_str), Some(log), clone_dir).with_fail_detail(&e)
    }
  }
}

/// Preserve a run's artifacts under its session's permanent home,
/// `$SCSH_HOME/sessions/<session>/`, before the run dir is pruned: the asciinema recording
/// to `casts/<stem>.cast` and the harness run log (plus any verbose `.debug`/`.last` logs)
/// to `logs/<stem>.{log,debug.log,last.log}`. All share one
/// `<skill>-<YYYYMMDD-HHMMSS>-utc-<nonce>` stem so a run's cast and logs correlate by name —
/// and the enclosing session dir names the run, so one `ls` finds everything a session made
/// and one `rm -rf` forgets exactly one run.
///
/// These live **outside** the caller repo on purpose: review skills often run in a throwaway
/// clone under `tmp/` and delete it afterward — recordings must still be exportable from the
/// session browser. Ordinary runs never delete under `sessions/`; use `scsh gc --apply`.
///
/// The timestamp alone is not unique (every skill in one `scsh run` shares `epoch_secs`), so
/// the random nonce prevents same-second runs from overwriting each other. Returns the durable
/// cast path (for the session browser) when a recording was copied.
fn persist_run_artifacts(session_id: &str, run_dir: &Path, skill_name: &str, epoch_secs: u64) -> Option<String> {
  let stem = format!("{skill_name}-{}-utc-{}", runtime::format_utc_timestamp(epoch_secs), runtime::random_nonce_6());

  // Logs: kept for every run (including failures, when they matter most). RUN_LOG_REL is the
  // teed harness output; `.debug` (claude/grok) and `.last` (codex) appear only in verbose runs.
  let logs_dir = runtime::session_logs_dir(session_id);
  if std::fs::create_dir_all(&logs_dir).is_ok() {
    for (rel, ext) in [
      (runtime::RUN_LOG_REL.to_string(), "log"),
      (format!("{}.debug", runtime::RUN_LOG_REL), "debug.log"),
      (format!("{}.last", runtime::RUN_LOG_REL), "last.log"),
      (format!("{}.exit", runtime::RUN_LOG_REL), "exit"),
      (format!("{}.tuidebug", runtime::RUN_LOG_REL), "tuidebug"),
    ] {
      let src = run_dir.join(&rel);
      if src.is_file() {
        let _ = std::fs::copy(&src, logs_dir.join(format!("{stem}.{ext}")));
      }
    }
  }

  // Cast: returned so the daemon can serve/replay/export it after the run dir (and any
  // throwaway caller clone) is gone.
  let cast_src = run_dir.join(runtime::RUN_CAST_REL);
  if !cast_src.is_file() {
    return None;
  }
  let casts_dir = runtime::session_casts_dir(session_id);
  std::fs::create_dir_all(&casts_dir).ok()?;
  let dest = casts_dir.join(format!("{stem}.cast"));
  std::fs::copy(&cast_src, &dest).ok()?;
  Some(dest.to_string_lossy().into_owned())
}

/// Recreate a skill's result file from a cached `content` (creating parent dirs), so a
/// cache hit leaves the same result on disk a real run would have collected.
fn restore_cached_result(root: &Path, result_rel: &str, content: &str) -> std::io::Result<()> {
  let dest = root.join(result_rel);
  if let Some(parent) = dest.parent() {
    std::fs::create_dir_all(parent)?;
  }
  std::fs::write(dest, content)
}

/// The first line of a (possibly multi-line) message, for a one-line skill report.
fn first_line(s: &str) -> &str {
  s.lines().next().unwrap_or(s)
}

/// `"s"` unless `n == 1`.
fn plural(n: usize) -> &'static str {
  if n == 1 {
    ""
  } else {
    "s"
  }
}

/// Age (seconds) past which a leftover `/tmp/scsh-*-run-*` clone is treated as stale and
/// swept at the next run's startup. A full day — comfortably longer than any skill run (skill
/// timeouts are in minutes) — so a concurrently-running scsh's fresh clone is never removed.
const STALE_RUN_DIR_SECS: u64 = 24 * 60 * 60;

/// Best-effort sweep of stale per-run clones left under `/tmp` by earlier runs — a failed
/// skill's kept clone, or a clone orphaned by a crash before cleanup. Only entries matching
/// [`runtime::is_scsh_run_dir_name`] AND older than [`STALE_RUN_DIR_SECS`] are removed,
/// so an in-progress concurrent run is never disturbed. Returns how many were removed.
fn sweep_stale_run_dirs(now: u64) -> usize {
  sweep_stale_run_dirs_in(Path::new("/tmp"), now, STALE_RUN_DIR_SECS)
}

/// The body of [`sweep_stale_run_dirs`], parameterized by the directory to scan and the
/// staleness threshold so it can be unit-tested. A matching entry is removed only if it is a
/// directory whose mtime is at least `max_age` seconds before `now`.
fn sweep_stale_run_dirs_in(dir: &Path, now: u64, max_age: u64) -> usize {
  let mut removed = 0;
  let Ok(entries) = std::fs::read_dir(dir) else {
    return 0;
  };
  for entry in entries.flatten() {
    let name = entry.file_name();
    let name = name.to_string_lossy();
    if !runtime::is_scsh_run_dir_name(&name) {
      continue;
    }
    let path = entry.path();
    if !path.is_dir() {
      continue;
    }
    let stale = std::fs::metadata(&path)
      .and_then(|m| m.modified())
      .ok()
      .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
      .map(|d| now.saturating_sub(d.as_secs()) >= max_age)
      .unwrap_or(false);
    if stale && std::fs::remove_dir_all(&path).is_ok() {
      removed += 1;
    }
  }
  removed
}

/// Create the per-run scratch dir under `/tmp`. Docker/podman use a UTC-stamped name with
/// `-2`, `-3`, … suffixes on collision; Apple `container` uses a random nonce and retries
/// with a fresh nonce when the dir already exists (container IDs must stay ≤ 64 chars).
fn prepare_run_dir(secs: u64, skill: &str, runtime: &str) -> Result<PathBuf, String> {
  if runtime == "container" {
    for _ in 1..=100 {
      let base = runtime::run_dir_name(secs, skill, runtime);
      let dir = PathBuf::from("/tmp").join(&base);
      match std::fs::create_dir(&dir) {
        Ok(()) => return Ok(dir),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
        Err(e) => return Err(format!("could not create run dir {}: {e}", dir.display())),
      }
    }
    return Err("could not create a unique run dir under /tmp".into());
  }
  let base = runtime::run_dir_name(secs, skill, runtime);
  for n in 1..=100 {
    let dir = PathBuf::from("/tmp").join(if n == 1 { base.clone() } else { format!("{base}-{n}") });
    match std::fs::create_dir(&dir) {
      Ok(()) => return Ok(dir),
      Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
      Err(e) => return Err(format!("could not create run dir {}: {e}", dir.display())),
    }
  }
  Err("could not create a unique run dir under /tmp".into())
}

/// Full clone (all history, all branches) of the host repo at `root` into the
/// already-created, empty `run_dir`, then materialize every remote branch as a
/// local one so the container sees them all. Used when bind-mounting the run dir
/// (Linux host → Linux container). Skills must not reach out to git remotes.
fn clone_into(
  root: &Path, run_dir: &Path, source_revision: Option<&str>, commit_identity: Option<&(String, String)>,
  base: Option<&RunBase>, spinner: &ui::screen::Proc,
) -> Result<(), String> {
  spinner.note("cloning…");
  let cmd = runtime::clone_command(&root.to_string_lossy(), &run_dir.to_string_lossy());
  let (ok, last) = spinner.run(&cmd[0], &cmd[1..]).map_err(|e| format!("failed to run git clone: {e}"))?;
  if !ok {
    return Err(match last {
      Some(l) if !l.is_empty() => format!("git clone failed: {l}"),
      _ => "git clone failed".to_string(),
    });
  }
  materialize_branches(run_dir);
  // After the local branches exist, move the mainline onto the requested base — the
  // container's `origin/<branch>` then resolves to exactly the commit the caller named.
  if let Some(base) = base {
    spinner.emit(&format!("base: {} → {}", base.branch, &base.sha[..base.sha.len().min(12)]));
    runtime::pin_clone_base(run_dir, base.branch, &base.sha)?;
  }
  if let Some(revision) = source_revision {
    checkout_workflow_revision(run_dir, revision)?;
  }
  set_clone_identity(run_dir, commit_identity);
  spinner.note("checking clone integrity…");
  let fsck = runtime::fsck_command(&run_dir.to_string_lossy());
  spinner.emit("git fsck --no-progress…");
  let fsck_started = Instant::now();
  let (ok, last) = spinner.run(&fsck[0], &fsck[1..]).map_err(|e| format!("failed to run git fsck: {e}"))?;
  let fsck_secs = fsck_started.elapsed().as_secs_f64();
  spinner.emit(&format!("git fsck {} ({})", if ok { "ok" } else { "failed" }, ui::clock::format_elapsed(fsck_secs),));
  if !ok {
    return Err(match last {
      Some(l) if !l.is_empty() => format!("git fsck failed on run clone: {l}"),
      _ => "git fsck failed on run clone".to_string(),
    });
  }
  Ok(())
}

/// Materialize a carried skill body when the delivery needs a file on disk. `DirectPrompt`
/// (harness-def `task:` / workflow `prompt:`) is a no-op — the text goes straight into the
/// harness CLI as a custom prompt. `GlobalInstall` (an override-bundle run) lands in the
/// harness's global skills dir under the run dir's `tmp/` — mounted on BOTH transports, so a
/// plain host-side write reaches the container and the checkout never contains the skill.
/// `Repo` is a no-op: the committed copy already rides in the clone.
fn materialize_skill_body(run_dir: &Path, _git_transport: bool, skill: &ResolvedInvocation) -> Result<(), String> {
  let write = |rel: &str, body: &str| -> Result<(), String> {
    let path = run_dir.join(rel);
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, body).map_err(|e| format!("could not write {}: {e}", path.display()))
  };
  match &skill.delivery {
    config::SkillDelivery::Repo | config::SkillDelivery::DirectPrompt(_) => Ok(()),
    config::SkillDelivery::GlobalInstall(body) => {
      write(&format!("{}/{}/SKILL.md", skill.harness.global_skills_rel(), skill.skill_source), body)
    }
  }
}

/// macOS Apple Container push IN: host `git push` into a bare transport repo; the container
/// clones from a short-lived `git daemon` (Linux-owned `.git`). Only `run_dir/tmp` is mounted.
fn prepare_git_transport(
  root: &Path, run_dir: &Path, commits: bool, source_revision: Option<&str>, base: Option<&RunBase>,
  spinner: &ui::screen::Proc,
) -> Result<(), String> {
  std::fs::create_dir_all(run_dir.join("tmp"))
    .map_err(|e| format!("could not create {}: {e}", run_dir.join("tmp").display()))?;
  spinner.note("pushing…");
  let bare = run_dir.join(runtime::TRANSPORT_BARE);
  runtime::push_transport_refs(root, &bare).map_err(|e| {
    spinner.emit(&format!("git push failed: {e}"));
    e
  })?;
  // The push mirrored the host's heads; overwrite the mainline with the requested review
  // base so the container clones a bare whose `<branch>` is already the base commit.
  if let Some(base) = base {
    spinner.emit(&format!("base: {} → {}", base.branch, &base.sha[..base.sha.len().min(12)]));
    runtime::pin_transport_base(&bare, base.branch, &base.sha)?;
  }
  if let Some(revision) = source_revision {
    select_transport_workflow_revision(&bare, revision)?;
  }
  if commits {
    let pull = run_dir.join(runtime::PULL_BARE);
    runtime::init_bare_repo(&pull)?;
    if source_revision.is_some() && !git_status_ok(&pull, &["symbolic-ref", "HEAD", "refs/heads/scsh-workflow"]) {
      return Err(format!("could not select the workflow branch in {}", pull.display()));
    }
  }
  Ok(())
}

/// Put a workflow run on the exact revision produced by its dependencies. A stable local
/// branch keeps commit-capable agents on a branch even when the producing step rewrote its
/// input history and scsh preserved that history under `scsh/incoming/*`.
fn checkout_workflow_revision(repo: &Path, revision: &str) -> Result<(), String> {
  if git_status_ok(repo, &["checkout", "--quiet", "-B", "scsh-workflow", revision]) {
    Ok(())
  } else {
    Err(format!("could not check out workflow revision {revision}"))
  }
}

/// Apple Container clones a bare transport instead of the host worktree. Point that bare
/// repository's HEAD at the same authoritative workflow revision used by bind-mount runs.
fn select_transport_workflow_revision(bare: &Path, revision: &str) -> Result<(), String> {
  if !git_status_ok(bare, &["update-ref", "refs/heads/scsh-workflow", revision]) {
    return Err(format!("workflow revision {revision} is not available in {}", bare.display()));
  }
  if !git_status_ok(bare, &["symbolic-ref", "HEAD", "refs/heads/scsh-workflow"]) {
    return Err(format!("could not select the workflow revision in {}", bare.display()));
  }
  Ok(())
}

/// Per-run `git daemon` serving `transport.git` (and optionally `pull.git`) from a run dir.
/// `--detach` is intentional: git's detached master closes every inherited descriptor before
/// accepting connections. Without it, a connection child can inherit a concurrently running
/// harness's stdout pipe and keep that harness stuck forever while scsh waits for EOF.
struct GitTransport {
  pid: u32,
  pid_file: PathBuf,
  port: u16,
}

impl GitTransport {
  fn start(run_dir: &Path) -> Result<Self, String> {
    let port = runtime::pick_ephemeral_port()?;
    let base = run_dir.to_string_lossy();
    let pid_file = run_dir.join("scsh-git-daemon.pid");
    let status = git_command()
      .args([
        "daemon",
        "--detach",
        "--reuseaddr",
        &format!("--base-path={base}"),
        "--export-all",
        "--enable=receive-pack",
        &format!("--port={port}"),
        "--listen=0.0.0.0",
        "--log-destination=none",
        &format!("--pid-file={}", pid_file.to_string_lossy()),
      ])
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status()
      .map_err(|e| format!("could not start git daemon: {e}"))?;
    if !status.success() {
      return Err(format!("git daemon exited with {status}"));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let pid = loop {
      if let Ok(pid) = std::fs::read_to_string(&pid_file).as_deref().map(str::trim).unwrap_or("").parse::<u32>() {
        break pid;
      }
      if Instant::now() >= deadline {
        return Err(format!("git daemon did not publish a valid PID to {} within 2s", pid_file.display()));
      }
      std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Self { pid, pid_file, port })
  }

  fn env(&self) -> Vec<(String, String)> {
    vec![(runtime::GIT_TRANSPORT_PORT_ENV.to_string(), self.port.to_string())]
  }
}

impl Drop for GitTransport {
  fn drop(&mut self) {
    let _ =
      Command::new("kill").arg("-TERM").arg(self.pid.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let _ = std::fs::remove_file(&self.pid_file);
  }
}

/// The deliberately unmistakable identity scsh stamps on commits a skill makes in its
/// clone — a "neon cyberpunk" bot that is never a real contributor. These commits are
/// LOCAL-ONLY by design (scsh rebases them onto your branch, it never pushes), so if this
/// author ever shows up in a code review or a pushed commit list, you pushed something
/// you shouldn't have. See `scsh help cache`.
pub(crate) const SCSH_COMMIT_NAME: &str = "dkorolev-neon-elon-bot";
pub(crate) const SCSH_COMMIT_EMAIL: &str = "dmitry.korolev+elon-presley@gmail.com";

/// The caller repo's effective git identity (`git config user.name` / `user.email`) — the
/// person running the pipeline. Steps declaring `commit-identity: runner` author their commits
/// as this identity; when either half is unset the caller falls back to the recognizable scsh
/// bot rather than guessing.
fn runner_commit_identity(root: &Path) -> Option<(String, String)> {
  let name = git_capture(root, &["config", "user.name"])?.trim().to_string();
  let email = git_capture(root, &["config", "user.email"])?.trim().to_string();
  (!name.is_empty() && !email.is_empty()).then_some((name, email))
}

/// Give the clone a *local* commit identity so a commit-enabled skill can `git commit`
/// inside the container — the mounted `.git/config` carries it, and the container's base
/// image has no global git identity. By default it is the deliberately recognizable
/// [`SCSH_COMMIT_NAME`] bot (see its docs); a step declaring `commit-identity: runner`
/// overrides it with the pipeline runner's own identity. Best-effort; failures never abort
/// the run. (Cherry-picking these commits back preserves this author; your own identity
/// becomes the committer.)
fn set_clone_identity(run_dir: &Path, identity: Option<&(String, String)>) {
  let (name, email) = identity.map(|(n, e)| (n.as_str(), e.as_str())).unwrap_or((SCSH_COMMIT_NAME, SCSH_COMMIT_EMAIL));
  let _ = git_capture(run_dir, &["config", "user.email", email]);
  let _ = git_capture(run_dir, &["config", "user.name", name]);
}

/// A resolved `--base <ref>`: which mainline branch a run pins, and the commit it pins it
/// to. Resolved ONCE on the host, before any clone, so every skill in the run measures from
/// the same commit and a bad ref fails the run instead of each container. Distinct from a
/// run's `caller_tip`, which is where the caller's branch stood when the run began and is
/// what commit-enabled steps rebase onto.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunBase {
  /// The branch repointed inside each run clone — `main`, or `master` when that is the
  /// repository's mainline. A skill reads it as `origin/<branch>` in the container.
  branch: &'static str,
  /// The full commit the branch is moved to.
  sha: String,
}

/// Resolve `--base <ref>` against the caller's repository. Every failure is refused
/// here, with the fix, rather than producing an empty or nonsensical diff in fifteen
/// containers: an unknown ref, a repository whose mainline is neither `main` nor `master`,
/// and the case where the checked-out branch IS the mainline (whose diff against itself is
/// empty by construction).
fn resolve_run_base(root: &Path, spec: &str) -> Result<RunBase, String> {
  let sha = git_capture(root, &["rev-parse", "--verify", "--quiet", &format!("{spec}^{{commit}}")])
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .ok_or_else(|| format!("--base '{spec}' is not a commit in this repository"))?;
  let heads = git_capture(root, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]).unwrap_or_default();
  let branch = runtime::base_branch(&heads)
    .ok_or_else(|| "--base needs a local 'main' or 'master' branch — this repository has neither".to_string())?;
  let current = git_capture(root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default().trim().to_string();
  if current == branch {
    return Err(format!("--base cannot be used while on '{branch}' — it is the branch being pinned"));
  }
  Ok(RunBase { branch, sha })
}

/// Best-effort: create a local branch for each `origin/*` branch the host-side
/// clone already has, so `git branch` in the container lists them all without any
/// fetch inside the container. Failures here never abort the run — the full history
/// is already present either way.
fn materialize_branches(run_dir: &std::path::Path) {
  let current = git_capture(run_dir, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
  let refs = match git_capture(run_dir, &["for-each-ref", "--format=%(refname:short)", "refs/remotes/origin"]) {
    Some(r) => r,
    None => return,
  };
  for b in runtime::local_branches_to_create(&refs, current.trim()) {
    let _ = git_command()
      .arg("-C")
      .arg(run_dir)
      .args(["branch", "--force", &b, &format!("origin/{b}")])
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status();
  }
}

// ---------------------------------------------------------------------------
// opencode credentials and config
//
// opencode in the container needs the host's login to talk to a model — especially
// custom/third-party providers configured in opencode (e.g. Nebius GLM). The image
// sets `XDG_DATA_HOME` to `repo/tmp/.xdg-data`. scsh copies the host's auth.json and opencode
// config (`~/.config/opencode/opencode.json`, optional `opencode.jsonc`) into each run clone
// under `tmp/.opencode-forward/` and bind-mounts from there — parallel runs cannot safely share
// one host bind-mount on Apple Containers. Opt out with `SCSH_NO_OPENCODE_AUTH=1`.
//
// Claude Code reads OAuth from `CLAUDE_CODE_OAUTH_TOKEN` (preferred — from `claude setup-token`)
// or `~/.claude/.credentials.json` (plus optional `~/.claude.json` / `~/.claude` config).
// scsh copies the host's Claude config into the run dir's gitignored tmp/ and bind-mounts
// it into the container; when the token env var is set it is also passed into the container
// and written as `.credentials.json` in the copy. Opt out with `SCSH_NO_CLAUDE_AUTH=1`.
// ---------------------------------------------------------------------------

/// Whether scsh forwards Claude credentials into runs (on unless opted out).
fn claude_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_CLAUDE_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh forwards opencode credentials into runs (on unless opted out).
fn opencode_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_OPENCODE_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh forwards codex credentials into runs (on unless opted out).
fn codex_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_CODEX_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh forwards grok credentials into runs (on unless opted out).
fn grok_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_GROK_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh forwards cursor credentials into runs (on unless opted out).
fn cursor_auth_enabled() -> bool {
  !matches!(std::env::var("SCSH_NO_CURSOR_AUTH").ok().as_deref(), Some("1") | Some("true"))
}

/// Whether scsh keeps every skill's `/tmp` run-clone instead of cleaning up. By default a
/// successful skill's clone is removed after the run (its result was collected and any commits
/// integrated) while a failed skill's clone is kept for inspection, and stale clones from past
/// runs are swept at startup. Set `SCSH_KEEP_RUNS=1` to keep all clones and skip the sweep.
fn keep_run_dirs() -> bool {
  matches!(std::env::var("SCSH_KEEP_RUNS").ok().as_deref(), Some("1") | Some("true"))
}

/// Tell the session-browser daemon to retry run-dir cleanup later if the client did not remove it.
fn schedule_run_dir_prune_backup(
  daemon_client: Option<&std::sync::Arc<daemon::Client>>, run_dir: &str, container_name: &str, runtime: &str,
  outcome_ok: bool,
) {
  if keep_run_dirs() {
    return;
  }
  if let Some(c) = daemon_client {
    c.schedule_run_dir_prune(run_dir, container_name, runtime, outcome_ok);
  }
}

/// Copy the host's opencode auth and config into `run_dir` for the upcoming run.
/// Copy the host's opencode auth + config INTO the run clone's gitignored `tmp/` — the image
/// points `XDG_DATA_HOME`/`XDG_CONFIG_HOME` at these paths, so they ride the repo mount (no
/// separate single-file bind mounts, which Docker Desktop on macOS rejects). Returns the auth
/// dir so the caller can scrub it after the run.
fn forward_opencode(run_dir: &Path) -> Option<PathBuf> {
  let home = std::env::var_os("HOME").map(PathBuf::from)?;
  let xdg_data = std::env::var_os("XDG_DATA_HOME");
  let xdg_config = std::env::var_os("XDG_CONFIG_HOME");
  let auth_src = runtime::opencode_auth_in(xdg_data.as_deref(), Some(home.as_os_str())).filter(|p| p.is_file())?;

  let data_dir = run_dir.join(runtime::OPENCODE_DATA_REL); // tmp/.xdg-data/opencode
  let cfg_dir = run_dir.join(runtime::OPENCODE_CONFIG_REL); // tmp/.config/opencode
  std::fs::create_dir_all(&data_dir).ok()?;
  std::fs::create_dir_all(&cfg_dir).ok()?;
  std::fs::copy(&auth_src, data_dir.join("auth.json")).ok()?;

  if let Some(cfg) = runtime::opencode_config_json_in(xdg_config.as_deref(), Some(home.as_os_str())) {
    let _ = std::fs::copy(&cfg, cfg_dir.join("opencode.json"));
  }
  if let Some(cfg) = runtime::opencode_config_jsonc_in(xdg_config.as_deref(), Some(home.as_os_str())) {
    let _ = std::fs::copy(&cfg, cfg_dir.join("opencode.jsonc"));
  }
  Some(data_dir)
}

/// Assemble the minimal Claude config the container needs into `run_dir`, returning the auth
/// root (so the caller can remove it afterward). The image's `CLAUDE_CONFIG_DIR` points at
/// this tree's `.claude` dir, so it rides along with the repo mount and stays writable.
///
/// Exactly two files are forwarded, both best-effort (a single copy failure never aborts the
/// others — deliberately NOT bulk-copying the host's real `~/.claude`, which holds history,
/// caches and unreadable junk and would fail partway):
///  - `.credentials.json` — host file if present, else the full macOS keychain blob, else a
///    minimal file from the env token. The interactive TUI treats an incomplete credentials
///    file (token only, no expiry/scopes/refresh) as logged out, so the complete blob matters.
///  - `.claude.json` — a MINIMAL state json: just the login identity (`oauthAccount`/`userID`)
///    lifted from the host's config so the TUI skips the login picker, plus the onboarding /
///    bypass-consent / repo-trust keys. The full host config is deliberately NOT forwarded —
///    its bulk (growthbook cache, history, install metadata) intermittently re-triggers the
///    bypass-permissions consent screen, while a minimal config does not.
fn forward_claude_auth(run_dir: &Path) -> Option<PathBuf> {
  let home = std::env::var_os("HOME").map(PathBuf::from);
  let token = runtime::claude_oauth_token();
  let keychain_creds = runtime::claude_keychain_credentials_json();
  let host_json = home.as_ref().map(|h| h.join(".claude.json")).filter(|p| p.is_file());
  let host_creds = home.as_ref().map(|h| h.join(".claude").join(".credentials.json")).filter(|p| p.is_file());

  if token.is_none() && keychain_creds.is_none() && host_creds.is_none() && host_json.is_none() {
    return None;
  }

  let root = run_dir.join(runtime::CLAUDE_AUTH_REL);
  let claude_dir = root.join(".claude");
  std::fs::create_dir_all(&claude_dir).ok()?;
  let creds_dest = claude_dir.join(".credentials.json");
  let json_dest = claude_dir.join(".claude.json");

  // Credentials, in order of completeness: host file > full keychain blob > env token.
  if let Some(src) = &host_creds {
    let _ = std::fs::copy(src, &creds_dest);
  } else if let Some(blob) = &keychain_creds {
    let _ = std::fs::write(&creds_dest, blob);
  } else if let Some(t) = &token {
    let _ = write_claude_credentials_file(&claude_dir, t);
  }

  // State json: forward ONLY the login identity from the host (not the whole config), then
  // seed the dialog-suppressing keys onto it.
  if let Some(src) = &host_json {
    write_claude_identity(src, &json_dest);
  }
  seed_claude_tui_config(&json_dest);

  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    for f in [&json_dest, &creds_dest] {
      if let Ok(p) = f.canonicalize() {
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
      }
    }
  }
  Some(root)
}

/// Write a minimal `.claude.json` at `dest` carrying only the host's Claude **login identity**
/// (`oauthAccount` + `userID`) — enough for the TUI to skip the login picker, without the rest
/// of the host config that would re-trigger the bypass consent. Best-effort; a missing/unparsable
/// host config just yields no identity (seed still writes the file).
fn write_claude_identity(host_json: &Path, dest: &Path) {
  use crate::json::Value;
  let Some(Value::Object(host)) = std::fs::read_to_string(host_json).ok().and_then(|t| json::parse(&t).ok()) else {
    return;
  };
  let mut identity: Vec<(String, Value)> = Vec::new();
  for key in ["oauthAccount", "userID"] {
    if let Some((_, v)) = host.iter().find(|(k, _)| k == key) {
      identity.push((key.to_string(), v.clone()));
    }
  }
  let _ = std::fs::write(dest, json::write(&Value::Object(identity)));
}

/// Add the keys that keep Claude Code's interactive TUI from blocking on first-run dialogs —
/// onboarding, bypass-permissions consent, and trust for the container repo path — onto the
/// minimal `.claude.json` from [`write_claude_identity`] ([`forward_claude_auth`]). A missing
/// file becomes a fresh minimal config, so it always exists and mounts.
fn seed_claude_tui_config(json_path: &Path) {
  use crate::json::Value;
  fn set(obj: &mut Vec<(String, Value)>, key: &str, val: Value) {
    if let Some(slot) = obj.iter_mut().find(|(k, _)| k == key) {
      slot.1 = val;
    } else {
      obj.push((key.to_string(), val));
    }
  }
  let mut root = match std::fs::read_to_string(json_path).ok().and_then(|t| json::parse(&t).ok()) {
    Some(Value::Object(o)) => o,
    _ => Vec::new(),
  };
  set(&mut root, "autoUpdates", Value::Bool(false));
  set(&mut root, "hasCompletedOnboarding", Value::Bool(true));
  // Suppress the bypass-permissions consent screen so the recorded TUI runs unattended.
  // The acceptance must be set at BOTH the top level and the repo's project entry — with only
  // one, the consent still appears (verified empirically). This lets `--permission-mode
  // bypassPermissions` auto-approve every tool with no prompt.
  set(&mut root, "bypassPermissionsModeAccepted", Value::Bool(true));
  let repo_project = Value::Object(vec![
    ("hasTrustDialogAccepted".to_string(), Value::Bool(true)),
    ("hasCompletedProjectOnboarding".to_string(), Value::Bool(true)),
    ("bypassPermissionsModeAccepted".to_string(), Value::Bool(true)),
  ]);
  let merged_into_existing = match root.iter_mut().find(|(k, _)| k == "projects") {
    Some((_, Value::Object(projects))) => {
      set(projects, runtime::AGENT_REPO, repo_project.clone());
      true
    }
    _ => false,
  };
  if !merged_into_existing {
    set(&mut root, "projects", Value::Object(vec![(runtime::AGENT_REPO.to_string(), repo_project)]));
  }
  let _ = std::fs::write(json_path, json::write(&Value::Object(root)));
}

/// Copy the host's Codex auth/config into the run clone's `tmp/.codex` (the image's
/// `CODEX_HOME`), returning that dir so the caller can scrub the credentials afterward.
/// No bind-mounts needed: the tree rides along with the repo/tmp mount in both mount modes.
fn forward_codex(run_dir: &Path) -> Option<PathBuf> {
  let host_home = runtime::codex_home_on_host()?;
  let auth = host_home.join("auth.json");
  let config = host_home.join("config.toml");
  if !auth.is_file() && !config.is_file() {
    return None;
  }
  let dest = run_dir.join(runtime::CODEX_FORWARD_REL);
  std::fs::create_dir_all(&dest).ok()?;
  for name in ["auth.json", "config.toml"] {
    let src = host_home.join(name);
    if src.is_file() {
      std::fs::copy(&src, dest.join(name)).ok()?;
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest.join(name), std::fs::Permissions::from_mode(0o600));
      }
    }
  }
  // Pre-trust the container repo path so codex's interactive TUI shows no folder-trust
  // prompt (appended, so a forwarded host config.toml keeps its other settings).
  {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dest.join("config.toml")) {
      let _ = write!(f, "\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n", runtime::AGENT_REPO);
    }
  }
  Some(dest)
}

/// Remove forwarded Codex credentials from a run dir, keeping codex's session/log data
/// (useful when a failed run dir is kept for inspection — tokens must not linger in /tmp).
fn scrub_codex_credentials(codex_dir: &Path) {
  for name in ["auth.json", "config.toml"] {
    let _ = std::fs::remove_file(codex_dir.join(name));
  }
}

/// Copy the host's Grok auth/config into the run clone's `tmp/.grok` (the image's
/// `GROK_HOME`), returning that dir so the caller can scrub the credentials afterward.
/// Same pattern as codex: no bind-mounts needed in either repo mount mode.
fn forward_grok(run_dir: &Path) -> Option<PathBuf> {
  let host_home = runtime::grok_home_on_host()?;
  let auth = host_home.join("auth.json");
  if !auth.is_file() {
    return None;
  }
  let dest = run_dir.join(runtime::GROK_FORWARD_REL);
  std::fs::create_dir_all(&dest).ok()?;
  // Forward grok's full credential + device identity so the container is the same logged-in
  // client as the host: `auth.json` (the OAuth session), `agent_id` (the device UUID the grok
  // gateway recognizes — without it the interactive Build TUI treats the container as a new
  // device and demands a browser sign-in), plus config/settings and `active_sessions.json`.
  for name in ["auth.json", "agent_id", "active_sessions.json", "config.toml", "user-settings.json"] {
    let src = host_home.join(name);
    if src.is_file() {
      std::fs::copy(&src, dest.join(name)).ok()?;
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest.join(name), std::fs::Permissions::from_mode(0o600));
      }
    }
  }
  Some(dest)
}

/// Remove forwarded Grok credentials from a run dir, keeping grok's session/log data.
fn scrub_grok_credentials(grok_dir: &Path) {
  for name in ["auth.json", "config.toml", "user-settings.json"] {
    let _ = std::fs::remove_file(grok_dir.join(name));
  }
}

/// Copy the host's Cursor config and OAuth tokens into the run clone's gitignored tmp/.
fn forward_cursor(run_dir: &Path) -> bool {
  let mut forwarded = false;
  if let Some(host_home) = runtime::cursor_home_on_host() {
    let dest = run_dir.join(runtime::CURSOR_FORWARD_REL);
    let mut any = false;
    for name in ["cli-config.json", "mcp.json"] {
      let src = host_home.join(name);
      if src.is_file() && std::fs::create_dir_all(&dest).is_ok() && std::fs::copy(&src, dest.join(name)).is_ok() {
        #[cfg(unix)]
        {
          use std::os::unix::fs::PermissionsExt;
          let _ = std::fs::set_permissions(dest.join(name), std::fs::Permissions::from_mode(0o600));
        }
        any = true;
      }
    }
    forwarded |= any;
  }
  let auth_dest = run_dir.join(runtime::CURSOR_AUTH_FORWARD_REL);
  if let Some(src) = runtime::cursor_auth_file_on_host() {
    if std::fs::create_dir_all(&auth_dest).is_ok() && std::fs::copy(&src, auth_dest.join("auth.json")).is_ok() {
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(auth_dest.join("auth.json"), std::fs::Permissions::from_mode(0o600));
      }
      forwarded = true;
    }
  } else if let Some(access) = runtime::cursor_keychain_access_token() {
    let refresh = runtime::cursor_keychain_refresh_token().unwrap_or_else(|| access.clone());
    if std::fs::create_dir_all(&auth_dest).is_ok() {
      let body = format!(r#"{{"accessToken":{},"refreshToken":{}}}"#, json::quote(&access), json::quote(&refresh));
      if std::fs::write(auth_dest.join("auth.json"), body).is_ok() {
        #[cfg(unix)]
        {
          use std::os::unix::fs::PermissionsExt;
          let _ = std::fs::set_permissions(auth_dest.join("auth.json"), std::fs::Permissions::from_mode(0o600));
        }
        forwarded = true;
      }
    }
  }
  forwarded
}

/// Remove forwarded Cursor credentials from a run dir, keeping session/log data.
fn scrub_cursor_credentials(run_dir: &Path) {
  for name in ["cli-config.json", "mcp.json"] {
    let _ = std::fs::remove_file(run_dir.join(runtime::CURSOR_FORWARD_REL).join(name));
  }
  let _ = std::fs::remove_file(run_dir.join(runtime::CURSOR_AUTH_FORWARD_REL).join("auth.json"));
}

fn write_claude_credentials_file(claude_dir: &Path, token: &str) -> Option<()> {
  let path = claude_dir.join(".credentials.json");
  let body = format!("{{\"claudeAiOauth\":{{\"accessToken\":{}}}}}", json::quote(token));
  std::fs::write(&path, body).ok()?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
  }
  Some(())
}

/// Resolve a skill's `env:` specs against the host environment into the
/// `(key, value)` pairs to forward into its container. `Err(message)` when a
/// required variable (`${VAR}`, `$VAR`, or `${VAR:?message}`) is unset — the skill
/// is refused before any work. A `${VAR:-default}` injects the host value or the
/// default; a constant is always forwarded.
fn resolve_env(env: &[config::EnvVar]) -> Result<Vec<(String, String)>, String> {
  use config::EnvRule;
  let mut out = Vec::new();
  for var in env {
    match &var.rule {
      EnvRule::Default { src, default } => {
        let value = std::env::var(src).unwrap_or_else(|_| default.clone());
        out.push((var.key.clone(), value));
      }
      EnvRule::Require { src, message } => match std::env::var(src) {
        Ok(v) => out.push((var.key.clone(), v)),
        Err(_) => {
          return Err(if message.is_empty() { format!("{src} is required but not set") } else { message.clone() });
        }
      },
      EnvRule::Constant(val) => out.push((var.key.clone(), val.clone())),
    }
  }
  Ok(out)
}

/// Pull the skill's `result` file OUT of the run clone into the caller repo (host-side,
/// after the container exits). Moves any pre-existing host file aside to
/// `<name>.bak.YYYYMMDD-HHMMSS-utc` first. This is always done for every skill — unlike
/// commits, which are pulled out only when `commits: true` and the skill committed.
fn collect_skill_result(root: &Path, run_dir: &Path, result: &str, secs: u64) -> Result<String, String> {
  let produced = run_dir.join(result);
  if !produced.is_file() {
    let ctx = failure::missing_result_context(run_dir, result);
    return Err(format!("did not produce its result file '{result}'{ctx}"));
  }
  let dest = root.join(result);
  if let Some(parent) = dest.parent() {
    std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
  }
  if dest.exists() {
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let backup = dest.with_file_name(runtime::backup_name(&name, secs));
    std::fs::rename(&dest, &backup).map_err(|e| format!("could not back up existing {}: {e}", dest.display()))?;
  }
  std::fs::copy(&produced, &dest).map_err(|e| format!("could not copy result to {}: {e}", dest.display()))?;
  Ok(dest.to_string_lossy().into_owned())
}

/// Run `git -C <dir> <args>` and return its trimmed stdout on success.
/// The ONE way scsh spawns `git`: a Command with inherited `GIT_DIR`/`GIT_WORK_TREE`/
/// `GIT_INDEX_FILE` stripped. git exports those to hook and `rebase --exec` children, and an
/// inherited `GIT_DIR` overrides `-C`/cwd discovery — scsh (or its test suite) running under
/// a hook would otherwise operate on the hook's repository instead of the one it targets.
pub(crate) fn git_command() -> std::process::Command {
  let mut cmd = std::process::Command::new("git");
  cmd.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env_remove("GIT_INDEX_FILE");
  cmd
}

/// Run `git -C <dir> <args>` and capture stdout. Inherited `GIT_DIR`/`GIT_WORK_TREE`/
/// `GIT_INDEX_FILE` are stripped: git exports them to hook and `rebase --exec` children, and
/// an inherited `GIT_DIR` overrides `-C` discovery — scsh invoked from a pre-push hook would
/// otherwise operate on the caller's repo instead of the one it was pointed at.
fn git_capture(dir: &std::path::Path, args: &[&str]) -> Option<String> {
  let out = git_command().arg("-C").arg(dir).args(args).output().ok()?;
  out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run `git -C <dir> <args>` for its exit status only, swallowing its output (so a
/// cherry-pick conflict doesn't spill onto the terminal). `true` on success.
fn git_status_ok(dir: &std::path::Path, args: &[&str]) -> bool {
  git_command().arg("-C").arg(dir).args(args).output().map(|o| o.status.success()).unwrap_or(false)
}

/// The caller repo's current branch name (for the "rebased onto <branch>" line);
/// falls back to "HEAD" when detached or unreadable.
fn current_branch(root: &Path) -> String {
  git_capture(root, &["rev-parse", "--abbrev-ref", "HEAD"])
    .map(|s| s.trim().to_string())
    .unwrap_or_else(|| "HEAD".into())
}

/// What happened when bringing a commit-enabled skill's commits back. `range` is the
/// `(from, to)` SHA pair in the CALLER repo that spans exactly the integrated commits —
/// what `pack_step_diff` renders into a review page. `None` when the before-state could
/// not be read (the diff is then simply not packed; the integration itself still counts).
enum Integration {
  /// The commits were rebased (cherry-picked) onto the caller's current branch.
  Applied { count: usize, range: Option<(String, String)> },
  /// They didn't apply cleanly, so they were saved to a distinct branch instead;
  /// the caller's branch was left untouched (the objects are fetched, so SHAs resolve).
  Saved { branch: String, count: usize, range: Option<(String, String)> },
}

/// Pull new commits OUT of a commit-enabled skill's run clone into the caller repo.
/// Called on the **host** after the container exits. Only when the skill declared
/// `commits: true` and actually added commits (`base..clone-HEAD` non-empty).
/// Uses `git fetch` from the **local run-clone path** — not from GitHub — then
/// cherry-picks onto the caller's current branch. Returns `None` when the skill added
/// no commits. scsh never pushes to any remote.
fn integrate_commits(
  root: &Path, run_dir: &Path, base: &str, skill: &str, stamp: &str,
) -> Result<Option<Integration>, String> {
  let source = runtime::commits_fetch_path(run_dir);
  // The skill's branch tip — what it left after (maybe) committing.
  let tip = match git_capture(&source, &["rev-parse", "HEAD"]) {
    Some(t) => t.trim().to_string(),
    None => return Err("could not read the clone's HEAD".into()),
  };
  if tip == base {
    return Ok(None); // the skill added nothing
  }
  // Make the skill's new objects available in the caller repo (host fetch from the local
  // run clone or pull.git bare repo — NOT from GitHub).
  let fetch_path = source.to_string_lossy();
  if !git_status_ok(root, &["fetch", "--no-tags", "--quiet", &fetch_path, "HEAD"]) {
    return Err("could not fetch the skill's commits from its clone".into());
  }
  let range = format!("{base}..{tip}");
  let count =
    git_capture(root, &["rev-list", "--count", &range]).and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(0);
  if count == 0 {
    return Ok(None);
  }
  // Try to rebase the range onto the caller's current branch. --keep-redundant-commits
  // preserves the side effect even if a commit's changes are already present (so the
  // "run twice = two commits" guarantee holds rather than collapsing to a no-op).
  let before = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
  if git_status_ok(root, &["cherry-pick", "--keep-redundant-commits", &range]) {
    let after = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
    Ok(Some(Integration::Applied { count, range: before.zip(after) }))
  } else {
    let _ = git_status_ok(root, &["cherry-pick", "--abort"]);
    let branch = incoming_branch_name(skill, stamp, &tip);
    if !git_status_ok(root, &["branch", "--force", &branch, &tip]) {
      return Err(format!("commits didn't rebase cleanly and the fallback branch '{branch}' could not be created"));
    }
    Ok(Some(Integration::Saved { branch, count, range: Some((base.to_string(), tip)) }))
  }
}

/// Pack the commits a step just brought in into one self-contained HTML review page
/// (`packdiff`, if it is on the PATH) under the session's durable artifact root
/// (`$SCSH_HOME/sessions/<session>/diffs/`), and register it with the daemon so the job
/// page grows a "⇄ commits diff" chip on that step's row. Best-effort residue: no packdiff,
/// no readable range, or a pack failure skips with a hint — never a run failure.
///
/// Invokes packdiff in machine mode (`--json`): stdout is a single `{ "Packed": … }` or
/// error document (packdiff 0.5.0). Progress stays on stderr and is discarded.
fn pack_step_diff(
  root: &Path, session_id: &str, skill: &ResolvedInvocation, outcome: &SkillRun, range: Option<(String, String)>,
  daemon_client: Option<&daemon::Client>,
) {
  let Some((from, to)) = range else {
    return;
  };
  let dir = runtime::session_diffs_dir(session_id);
  if std::fs::create_dir_all(&dir).is_err() {
    return;
  }
  let out = dir.join(format!("{}-p{}.html", runtime::sanitize_component(&skill.name), outcome.proc_index));
  let title = format!("scsh job {session_id} · {} commits", skill.name);
  // `..` = the literal range: `from` is the branch tip the commits landed on (or their
  // merge base on the saved-branch path), so merge-base resolution has nothing to add.
  // The notes author is pinned to scsh's own bot: a skill-committed PR-DESCRIPTION.md is
  // bot-authored (every clone commit is), and packdiff lifts the notes author's
  // description into the page's Description panel — regardless of the host env.
  let result = Command::new("packdiff")
    .arg(format!("{from}..{to}"))
    .args(["-C", &root.to_string_lossy(), "-o", &out.to_string_lossy(), "--title", &title, "--json"])
    .env("PACKDIFF_SYSTEM_USER_EMAIL", SCSH_COMMIT_EMAIL)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .output();
  match result {
    Ok(output) if output.status.success() && out.is_file() => {
      ok(&format!("{}: commits diff packed — {}", skill.name, out.display()));
      if let Some(c) = daemon_client {
        c.proc_diff(outcome.proc_index, &out.to_string_lossy());
      }
    }
    Ok(output) => {
      let detail = packdiff_failure_detail(&output.stdout);
      hint(&format!(
        "{}: packdiff could not pack the commits diff (skipped{})",
        skill.name,
        detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default()
      ));
    }
    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
      hint(
        "packdiff not found — `cargo install packdiff --version 0.5.0 --locked` to browse each step's commits from the job page",
      );
    }
    Err(e) => hint(&format!("{}: packdiff failed to start — {e}", skill.name)),
  }
}

fn pack_job_diff(root: &Path, session_id: &str, from: &str, to: &str) {
  if from == to {
    return;
  }
  let dir = runtime::session_diffs_dir(session_id);
  if std::fs::create_dir_all(&dir).is_err() {
    return;
  }
  let out = dir.join("job.html");
  let next = dir.join("job.next.html");
  let title = format!("scsh job {session_id} · all commits");
  let result = Command::new("packdiff")
    .arg(format!("{from}..{to}"))
    .args(["-C", &root.to_string_lossy(), "-o", &next.to_string_lossy(), "--title", &title, "--json"])
    .env("PACKDIFF_SYSTEM_USER_EMAIL", SCSH_COMMIT_EMAIL)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .output();
  if result.is_ok_and(|output| output.status.success() && next.is_file()) {
    if std::fs::rename(&next, &out).is_ok() {
      ok(&format!("whole-job commits diff packed — {}", out.display()));
    }
  } else {
    let _ = std::fs::remove_file(next);
  }
}

/// Pull a short reason out of packdiff's machine-mode error document (or `None`).
fn packdiff_failure_detail(stdout: &[u8]) -> Option<String> {
  let text = std::str::from_utf8(stdout).ok()?.trim();
  if text.is_empty() {
    return None;
  }
  // Prefer the typed `message` field when present; otherwise the top-level variant name.
  if let Some(i) = text.find("\"message\"") {
    let after = &text[i + "\"message\"".len()..];
    if let Some(rest) = after.trim_start().strip_prefix(':') {
      let rest = rest.trim_start();
      if let Some(s) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
          match c {
            '\\' => match chars.next() {
              Some('n') => out.push('\n'),
              Some('t') => out.push('\t'),
              Some('"') => out.push('"'),
              Some('\\') => out.push('\\'),
              Some(o) => {
                out.push('\\');
                out.push(o);
              }
              None => break,
            },
            '"' => break,
            other => out.push(other),
          }
        }
        if !out.is_empty() {
          return Some(out);
        }
      }
    }
  }
  // `{ "UnknownRef": { ... } }` → `UnknownRef`
  let trimmed = text.trim_start().trim_start_matches('{').trim_start();
  if let Some(key) = trimmed.strip_prefix('"') {
    let name: String = key.chars().take_while(|c| *c != '"').collect();
    if !name.is_empty() && name != "Packed" {
      return Some(name);
    }
  }
  None
}

/// A distinct branch name for commits that couldn't be rebased cleanly:
/// `scsh/incoming/<skill>-<stamp>-<short>` — the UTC stamp plus the tip's short hash,
/// so the user can see exactly what the branch carries.
fn incoming_branch_name(skill: &str, stamp: &str, tip: &str) -> String {
  let short: String = tip.chars().take(7).collect();
  format!("scsh/incoming/{}-{}-utc-{}", runtime::sanitize_component(skill), stamp, short)
}

// ---------------------------------------------------------------------------
// Result cache (content-addressed, under the repo's gitignored tmp/.sccache/)
// ---------------------------------------------------------------------------

/// Where cached results live: `.sccache/` under the repo's gitignored scratch root (`tmp/`, or
/// `.harness/tmp` when that is the gitignored one).
fn cache_dir(root: &Path) -> PathBuf {
  root.join(scratch_root(root).unwrap_or("tmp")).join(".sccache")
}

/// The cache key for a skill run: a sha256 over a deterministic blob of the repo's
/// committed content (the HEAD tree hash), the skill's own files (`SKILL.md` + scripts,
/// each hashed, in sorted order), and the resolved env (sorted). So the **same commit +
/// same skill + same env** map to the same key. `None` when the repo content can't be
/// read (e.g. a repo with no commit yet) — then the run is simply not cached.
#[cfg(test)]
fn cache_key(root: &Path, skill: &ResolvedInvocation, env: &[(String, String)]) -> Option<String> {
  cache_key_at(root, skill, env, None, None)
}

/// Workflow variant of [`cache_key`]: hash the authoritative carried revision rather than
/// the caller branch, which may intentionally remain untouched after a history rewrite.
fn cache_key_at(
  root: &Path, skill: &ResolvedInvocation, env: &[(String, String)], source_revision: Option<&str>,
  base: Option<&RunBase>,
) -> Option<String> {
  // Declared artifacts are side FILES; the cache journals only the result content (plus
  // commits), so a hit could not reproduce them — artifact-bearing steps always run live.
  if !skill.artifacts.is_empty() {
    return None;
  }
  let treeish = source_revision.map(|r| format!("{r}^{{tree}}")).unwrap_or_else(|| "HEAD^{tree}".to_string());
  let tree = git_capture(root, &["rev-parse", &treeish])?.trim().to_string();
  let mut blob = String::new();
  blob.push_str("scsh-cache v2\n");
  blob.push_str(&format!("repo-tree={tree}\n"));
  blob.push_str(&format!("invocation={}\n", skill.name));
  blob.push_str(&format!("skill={}\n", skill.skill_source));
  blob.push_str(&format!("harness={}\n", skill.harness.as_str()));
  blob.push_str(&format!("model={}\n", skill.model.as_deref().unwrap_or("")));
  blob.push_str(&format!("effort={}\n", skill.effort.as_deref().unwrap_or("")));
  blob.push_str("skill-files:\n");
  for (rel, hash) in skill_file_hashes(root, &skill.skill_source) {
    blob.push_str(&format!("{rel} {hash}\n"));
  }
  // For a harness-definition/workflow run the body isn't a caller `.skills/` file — it rides on
  // the invocation — so hash it here, so changing a definition's prompt busts the cache.
  if let Some(body) = skill.delivery.body() {
    blob.push_str(&format!("body={}\n", sha256::sha256_hex(body.as_bytes())));
  }
  // A pinned base changes what the skill SEES (`origin/<branch>..HEAD`) without
  // changing the repo tree, so it must key the cache. Appended only when pinning is in play,
  // so keys cached by ordinary runs stay valid.
  if let Some(base) = base {
    blob.push_str(&format!("base={}={}\n", base.branch, base.sha));
  }
  // Hash the resolved input environment — every param and (for a workflow) every value bound
  // from an upstream step's output. This is what makes different inputs a cache MISS. SCSH_RESULT
  // is excluded: it is the output *path* (which for a workflow carries the random session id), not
  // an input, so it must not change the key.
  blob.push_str("env:\n");
  let mut pairs: Vec<&(String, String)> = env.iter().filter(|(k, _)| k != "SCSH_RESULT").collect();
  pairs.sort_by(|a, b| a.0.cmp(&b.0));
  for (k, v) in pairs {
    blob.push_str(&format!("{k}={v}\n"));
  }
  Some(sha256::sha256_hex(blob.as_bytes()))
}

/// `(repo-relative path, sha256-of-content)` for every file under `.skills/<name>/`,
/// sorted by path — a deterministic fingerprint of the skill body and its scripts.
fn skill_file_hashes(root: &Path, name: &str) -> Vec<(String, String)> {
  let dir = root.join(".skills").join(name);
  let mut found: Vec<(String, PathBuf)> = Vec::new();
  collect_files(&dir, &dir, &mut found);
  found.sort();
  found.into_iter().map(|(rel, abs)| (rel, sha256::sha256_hex(&std::fs::read(&abs).unwrap_or_default()))).collect()
}

/// Recursively collect `(path-relative-to-base, absolute-path)` for every regular file
/// under `dir`. Order is not guaranteed (the caller sorts).
fn collect_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
  let entries = match std::fs::read_dir(dir) {
    Ok(e) => e,
    Err(_) => return,
  };
  for entry in entries.flatten() {
    let path = entry.path();
    if path.is_dir() {
      collect_files(base, &path, out);
    } else if path.is_file() {
      if let Ok(rel) = path.strip_prefix(base) {
        out.push((rel.to_string_lossy().replace('\\', "/"), path));
      }
    }
  }
}

/// A cached run — everything needed to reproduce the original observation, not just the answer:
/// the result-file content, any commits it made (a `format-patch` mbox, for a commit-enabled
/// skill), how long the original run took, when it was cached, its terminal recording, and
/// (when annotation has finished) the chapters sidecar. A hit restores the replayable video and
/// chapters while labeling their source duration separately from the cache-hit attempt's clock.
struct CacheEntry {
  result: String,
  commits: Option<String>,
  /// The original run's wall-clock seconds, shown as provenance for its restored recording.
  elapsed: Option<f64>,
  /// Unix seconds when this entry was written (the original run's finish time).
  cached_at: Option<u64>,
  /// The original run's cast recording (copied into the cache), for replay on a hit.
  cast: Option<PathBuf>,
  /// Chapters sidecar next to the cached cast (`<key>.chapters.json`), when annotation has
  /// landed (either at store time or attached later via [`cache_attach_chapters`]).
  chapters: Option<PathBuf>,
}

/// Look up the cache entry for `key` (result, journaled commits, original duration, and cast).
fn cache_lookup(root: &Path, key: &str) -> Option<CacheEntry> {
  let dir = cache_dir(root);
  let json_path = dir.join(format!("{key}.json"));
  let text = std::fs::read_to_string(&json_path).ok()?;
  let cast = dir.join(format!("{key}.cast"));
  let chapters = dir.join(format!("{key}.chapters.json"));
  // Prefer the stamped `cached_at`; fall back to the entry file's mtime for older caches.
  let cached_at = json::field(&text, "cached_at").and_then(|s| s.trim().parse::<u64>().ok()).or_else(|| {
    std::fs::metadata(&json_path)
      .ok()
      .and_then(|m| m.modified().ok())
      .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
      .map(|d| d.as_secs())
  });
  Some(CacheEntry {
    result: json::field(&text, "result")?,
    commits: json::field(&text, "commits"),
    elapsed: json::field(&text, "elapsed").and_then(|s| s.trim().parse::<f64>().ok()),
    cached_at,
    cast: cast.is_file().then_some(cast),
    chapters: chapters.is_file().then_some(chapters),
  })
}

/// Store a skill's result-file `content`, any commit `patch`, the original `elapsed` seconds, and
/// its `cast_src` recording in the cache under `key` (the cast is copied alongside as
/// `<key>.cast`; chapters as `<key>.chapters.json` when already present). Best-effort: a write
/// failure just means the next identical run won't be a hit.
fn cache_store(
  root: &Path, key: &str, content: &str, commits: Option<&str>, elapsed: Option<f64>, cast_src: Option<&Path>,
) {
  let dir = cache_dir(root);
  if std::fs::create_dir_all(&dir).is_err() {
    return;
  }
  // elapsed / cached_at are stored as quoted strings so the string-only `json::field` reader
  // can read them back.
  let mut fields = vec![format!("\"result\": {}", json::quote(content))];
  if let Some(patch) = commits {
    fields.push(format!("\"commits\": {}", json::quote(patch)));
  }
  if let Some(e) = elapsed {
    fields.push(format!("\"elapsed\": {}", json::quote(&format!("{e}"))));
  }
  fields.push(format!("\"cached_at\": {}", json::quote(&format!("{}", now_secs()))));
  let _ = std::fs::write(dir.join(format!("{key}.json")), format!("{{{}}}\n", fields.join(", ")));
  if let Some(src) = cast_src.filter(|p| p.is_file()) {
    let _ = std::fs::copy(src, dir.join(format!("{key}.cast")));
    // Remember which cache key this durable cast feeds, so post-run annotation can attach
    // chapters onto the cached copy (annotation finishes after cache_store).
    let _ = std::fs::write(PathBuf::from(format!("{}.sccache-key", src.display())), key);
    if let Some(chapters) = daemon::chapters_sidecar_path(&src.to_string_lossy()) {
      if chapters.is_file() {
        let _ = std::fs::copy(&chapters, dir.join(format!("{key}.chapters.json")));
      }
    }
  }
}

/// After a cast's chapters sidecar appears (annotation), copy it into the result cache next to
/// the matching `{key}.cast` — so a later cache hit restores chapters with the recording.
/// Uses the `{cast}.sccache-key` marker written by [`cache_store`]. Best-effort no-op otherwise.
fn cache_attach_chapters(root: &Path, cast: &Path) {
  let Some(chapters) = daemon::chapters_sidecar_path(&cast.to_string_lossy()) else {
    return;
  };
  if !chapters.is_file() {
    return;
  }
  let key_path = PathBuf::from(format!("{}.sccache-key", cast.display()));
  let Ok(key) = std::fs::read_to_string(&key_path) else {
    return;
  };
  let key = key.trim();
  if key.is_empty() {
    return;
  }
  let dest = cache_dir(root).join(format!("{key}.chapters.json"));
  let _ = std::fs::copy(&chapters, dest);
}

/// Human stamp for a cache hit: `2026-07-09 21:35 UTC` from a unix-seconds `cached_at`.
fn format_cached_at(epoch_secs: u64) -> String {
  let raw = runtime::format_utc_timestamp(epoch_secs); // YYYYMMDD-HHMMSS
  if raw.len() >= 15 {
    format!("{}-{}-{} {}:{} UTC", &raw[0..4], &raw[4..6], &raw[6..8], &raw[9..11], &raw[11..13])
  } else {
    raw
  }
}

/// Human-readable provenance attached to a cache-hit result. The cache-hit proc's `elapsed`
/// remains its own lookup/restore duration; this text explains why its restored recording can
/// legitimately be much longer.
fn cache_hit_provenance(cached_at: Option<u64>, source_elapsed: Option<f64>) -> String {
  let mut parts = vec!["cached".to_string()];
  if let Some(at) = cached_at {
    parts.push(format_cached_at(at));
  }
  if let Some(elapsed) = source_elapsed {
    parts.push(format!("source run took {}", ui::clock::format_elapsed(elapsed)));
  }
  parts.join(" · ")
}

/// A commit-enabled skill's new commits in its clone (`base..HEAD`) as a git `format-patch`
/// mbox, or `None` if it committed nothing. Stored in the cache so a hit can replay them.
fn commit_patch(clone: &Path, base: &str) -> Option<String> {
  let out = git_capture(clone, &["format-patch", &format!("{base}..HEAD"), "--stdout"])?;
  if out.trim().is_empty() {
    None
  } else {
    Some(out)
  }
}

/// Replay commits journaled in the cache (a `format-patch` mbox) onto the caller's current
/// branch via `git am`, so a cache hit reproduces the commit side effect. Returns `Applied`
/// on a clean replay; if the patch doesn't apply, aborts and saves it under tmp/.sccache for
/// the user to apply by hand (reported via the `Err` path), leaving the branch untouched.
fn apply_cached_commits(root: &Path, patch: &str, skill: &str, stamp: &str) -> Result<Option<Integration>, String> {
  let before = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
  let staged = std::env::temp_dir().join(format!("scsh-replay-{}-{stamp}.patch", std::process::id()));
  std::fs::write(&staged, patch).map_err(|_| "could not stage the cached commits".to_string())?;
  let applied = git_status_ok(root, &["am", "--keep-cr", &staged.to_string_lossy()]);
  let _ = std::fs::remove_file(&staged);
  if applied {
    let count = before
      .as_deref()
      .and_then(|b| git_capture(root, &["rev-list", "--count", &format!("{b}..HEAD")]))
      .and_then(|s| s.trim().parse::<usize>().ok())
      .unwrap_or(1);
    let after = git_capture(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string());
    return Ok(Some(Integration::Applied { count, range: before.zip(after) }));
  }
  let _ = git_status_ok(root, &["am", "--abort"]);
  let saved = cache_dir(root).join(format!("incoming-{}-{stamp}.patch", runtime::sanitize_component(skill)));
  let _ = std::fs::create_dir_all(cache_dir(root));
  let _ = std::fs::write(&saved, patch);
  Err(format!(
    "cached commits didn't apply cleanly — saved the patch to {} (apply with: git am <file>)",
    saved.display()
  ))
}

/// Seconds since the Unix epoch (UTC), for run-dir and backup timestamps.
fn now_secs() -> u64 {
  std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Build a single harness image as a recorded TUI — scsh re-executes itself as the PTY
/// recorder (`__record-pty`), so docker/podman/Apple `container` all render their native
/// progress under a real PTY and EVERY build is a cast on every machine, no host tooling
/// required. The cast is registered with the daemon so the session page embeds the player.
/// `no_cache` forces every layer to rebuild (`build-images --force` / `--rebuild-base`).
#[allow(clippy::too_many_arguments)]
fn run_build(
  build: &ui::screen::Proc, runtime_name: &str, tag: &str, target: &str, dockerfile: &str, uid: u32, gid: u32,
  fingerprint: &str, no_cache: bool, daemon_client: Option<&daemon::Client>, cast_stem: &str, session_id: &str,
) -> Result<(), (String, i32)> {
  let tz = runtime::host_timezone();

  if runtime_name == "container" && runtime::apple_dockerfile_too_large(dockerfile) {
    return Err((runtime::apple_dockerfile_too_large_message(dockerfile.len()), 1));
  }

  run_build_tui(
    build,
    runtime_name,
    tag,
    target,
    dockerfile,
    uid,
    gid,
    &tz,
    fingerprint,
    no_cache,
    daemon_client,
    cast_stem,
    session_id,
  )
}

fn image_build_failure(
  runtime_name: &str, target: &str, tag: &str, build: &ui::screen::Proc, last: Option<&str>,
) -> (String, i32) {
  let tail = build.tail_lines(failure::FAILURE_TAIL_LINES);
  let excerpt = failure::failure_excerpt(last, &tail, "build produced no output");
  let detail = if runtime_name == "container" {
    runtime::rewrite_apple_build_failure(&excerpt).unwrap_or(excerpt)
  } else {
    excerpt
  };
  (format!("image build failed (runtime={runtime_name}, target={target}, tag={tag}): {detail}"), 1)
}

/// Record one image build under a host PTY via scsh's own recorder (`__record-pty`),
/// register the cast with the daemon, and return Ok/Err from the builder's exit status.
#[allow(clippy::too_many_arguments)]
fn run_build_tui(
  build: &ui::screen::Proc, runtime_name: &str, tag: &str, target: &str, dockerfile: &str, uid: u32, gid: u32,
  tz: &str, fingerprint: &str, no_cache: bool, daemon_client: Option<&daemon::Client>, cast_stem: &str,
  session_id: &str,
) -> Result<(), (String, i32)> {
  let casts_dir = runtime::session_casts_dir(session_id);
  std::fs::create_dir_all(&casts_dir)
    .map_err(|e| (format!("could not create cast dir {}: {e}", casts_dir.display()), 1))?;
  let cast_path = casts_dir.join(format!(
    "build-{cast_stem}-{}-utc-{}.cast",
    runtime::format_utc_timestamp(now_secs()),
    runtime::random_nonce_6()
  ));
  let cast_path_str = cast_path.to_string_lossy().into_owned();

  // Context dir for every runtime: a PTY-recorded shell cannot feed Dockerfile-on-stdin the
  // way Proc::run_with_stdin does, and Apple container already requires a context dir.
  let dir = make_temp_dir().map_err(|e| (format!("could not create build context: {e}"), 1))?;
  let df_path = dir.join(runtime::CONTEXT_DOCKERFILE_NAME);
  if let Err(e) = std::fs::write(&df_path, dockerfile) {
    let _ = std::fs::remove_dir_all(&dir);
    return Err((format!("could not write Dockerfile to build context: {e}"), 1));
  }
  let build_argv = runtime::build_command_context(
    runtime_name,
    tag,
    target,
    &dir.to_string_lossy(),
    uid,
    gid,
    tz,
    fingerprint,
    no_cache,
  );
  let term = config::Terminal::default();
  // scsh records the PTY itself — argv passes through verbatim after `--`, no shell join.
  let exe = std::env::current_exe().map_err(|e| (format!("could not find the scsh binary: {e}"), 1))?;
  let mut rec: Vec<String> = vec![
    exe.to_string_lossy().into_owned(),
    "__record-pty".into(),
    "--cast".into(),
    cast_path_str.clone(),
    "--cols".into(),
    term.cols.to_string(),
    "--rows".into(),
    term.rows.to_string(),
    "--".into(),
  ];
  rec.extend(build_argv);

  if let Some(c) = daemon_client {
    // Register before the recorder starts so the session page can open the player mid-build.
    c.proc_cast(build.index(), &cast_path_str);
  }

  let started = |e: std::io::Error| {
    let _ = std::fs::remove_dir_all(&dir);
    (format!("failed to start the build recorder: {e}"), 1)
  };
  // Quiet pump: the cast is the UI; we still capture a short tail for failure excerpts.
  let (ok, last) = build.run(&rec[0], &rec[1..]).map_err(started)?;
  let _ = std::fs::remove_dir_all(&dir);

  if let Some(c) = daemon_client {
    // Re-register in case the path was cleared; durable path is already under sessions/<id>/casts/.
    c.proc_cast(build.index(), &cast_path_str);
  }

  if ok {
    Ok(())
  } else {
    Err(image_build_failure(runtime_name, target, tag, build, last.as_deref()))
  }
}

fn make_temp_dir() -> std::io::Result<PathBuf> {
  let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
  let dir = std::env::temp_dir().join(format!("scsh-build-{}-{nanos}", std::process::id()));
  std::fs::create_dir_all(&dir)?;
  Ok(dir)
}

/// `scsh build-images [harness…] [--force] [--rebuild-base] [--session <id>]` — build the shared
/// base image and the selected harness images outside any run, streaming into the live board and
/// the session browser exactly like a run's build rows. No git repo or `.scsh.yml` is needed —
/// the Dockerfile is embedded. The daemon's images panel spawns this command on its Build
/// buttons, passing `--session` so the browser can deep-link the pre-created session.
///
/// Semantics: stale images (fingerprint mismatch) always rebuild. `--force` rebuilds the
/// selected harness images even when up to date (`--no-cache`). `--rebuild-base` force-rebuilds
/// the base with `--no-cache` and then rebuilds every selected harness image on top of it
/// (cached — their layers chain from the fresh base, so they re-run where it matters).
fn build_images_cmd(names: &[String], force: bool, rebuild_base: bool, session: Option<String>) -> i32 {
  ui::signals::install();
  // `base` is a first-class name: `scsh build-images base` builds ONLY the shared base
  // image (respecting its fingerprint unless --force / --rebuild-base).
  let mut base_only = false;
  let mut selected: Vec<config::Harness> = Vec::new();
  if names.is_empty() {
    selected.extend(config::Harness::ALL);
  } else {
    for n in names {
      if n == "base" {
        base_only = true;
        continue;
      }
      match config::Harness::parse(n) {
        Some(h) if selected.contains(&h) => {}
        Some(h) => selected.push(h),
        None => {
          fail(&format!("unknown image '{n}' (known: base, {})", config::Harness::known().join(", ")));
          return 2;
        }
      }
    }
  }

  let rt = match runtime::detect_runtime() {
    Some(rt) => rt,
    None => {
      fail("no container runtime found (docker, podman, or Apple `container`)");
      return 1;
    }
  };
  if !ui::engine::is_running(&rt.name) {
    fail(&format!("{} is installed but not running", ui::engine::display_name(&rt.name)));
    if let Some(cmd) = ui::engine::start_command(&rt.name, ui::Os::current()) {
      hint(&format!("start it with: {}", bold(&cmd)));
    }
    return 1;
  }

  // Session browser wiring — same shape as a run; `--session` reuses the id the daemon
  // pre-created so its Build button can link to the session before this process starts.
  let session_id = session.filter(|s| !s.is_empty()).unwrap_or_else(daemon::new_session_id);
  let mut daemon_session = DaemonSession { client: None, ping_active: None, registered: false };
  match daemon::ensure_for_run() {
    Ok(()) => {
      let client = std::sync::Arc::new(daemon::Client::new(session_id.clone()));
      if client.register_session("(image builds)", "", Some("build-images"), "build", &[]) {
        ok(&format!("track progress at {}", client.session_url()));
        let ping_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ping_flag = std::sync::Arc::clone(&ping_active);
        let ping_client = std::sync::Arc::clone(&client);
        std::thread::spawn(move || {
          while ping_flag.load(std::sync::atomic::Ordering::Relaxed) {
            ping_client.ping();
            std::thread::sleep(Duration::from_secs(2));
          }
        });
        daemon_session.client = Some(client);
        daemon_session.ping_active = Some(ping_active);
        daemon_session.registered = true;
      }
    }
    Err(e) => hint(&format!("session browser daemon unavailable ({e}); continuing without live browser UI")),
  }
  let daemon_client = daemon_session.client.clone();
  let ui = ui::screen::LiveUi::new(console::user_attended_stderr(), daemon_client.clone());

  // Apple Containers: comment-strip so the Dockerfile fits the gRPC header limit (#735).
  let df = runtime::dockerfile_for_runtime(&rt.name);
  if rt.name == "container" && runtime::apple_dockerfile_too_large(&df) {
    fail(&runtime::apple_dockerfile_too_large_message(df.len()));
    return 1;
  }
  let (uid, gid) = runtime::host_ids();
  let tz = runtime::host_timezone();
  let rt_name = rt.name.clone();

  let base_fp = runtime::base_image_fingerprint(&df, uid, gid, &tz);
  let base_stale = !runtime::image_is_up_to_date(&rt_name, runtime::BASE_IMAGE_TAG, &base_fp);
  // An explicit `base` in the names builds the base even when fresh under --force, and by
  // itself builds ONLY the base (selected stays empty).
  let build_base = base_stale || rebuild_base || (base_only && force);
  let mut harness_builds: Vec<runtime::ImageBuildSpec> = Vec::new();
  for &h in &selected {
    let spec = runtime::image_build_spec(h, &df, uid, gid, &tz);
    if force || rebuild_base || !runtime::image_is_up_to_date(&rt_name, &spec.tag, &spec.fingerprint) {
      harness_builds.push(spec);
    } else {
      ok(&format!("{} up to date", spec.tag));
    }
  }
  if !build_base {
    ok(&format!("{} up to date", runtime::BASE_IMAGE_TAG));
  }
  if !build_base && harness_builds.is_empty() {
    ui.finish();
    ok("nothing to build — every selected image is up to date (use --force to rebuild anyway)");
    return 0;
  }

  let mut base_build = None;
  if build_base {
    let label = format!("using {} · build base", backend_name(&rt_name));
    let builder = format!("{} build", backend_name(&rt_name));
    let p = ui.proc(label.clone(), false);
    if let Some(c) = &daemon_client {
      c.proc_add(p.index(), &label, daemon::ProcKind::Build, None, Some(&builder), None, None, None, None, None);
    }
    base_build = Some(p);
  }
  let mut harness_build_procs: Vec<ui::screen::Proc> = Vec::with_capacity(harness_builds.len());
  for spec in &harness_builds {
    let label = format!("using {} · build {}", backend_name(&rt_name), spec.harness.as_str());
    let builder = format!("{} build", backend_name(&rt_name));
    let p = ui.proc(label.clone(), false);
    if let Some(c) = &daemon_client {
      c.proc_add(p.index(), &label, daemon::ProcKind::Build, None, Some(&builder), None, None, None, None, None);
    }
    harness_build_procs.push(p);
  }
  ui.pin_board_to_top();

  let mut build_failed = if let Some(ref base) = base_build {
    base.start();
    match run_build(
      base,
      &rt_name,
      runtime::BASE_IMAGE_TAG,
      runtime::BASE_IMAGE_TARGET,
      &df,
      uid,
      gid,
      &base_fp,
      rebuild_base,
      daemon_client.as_deref(),
      "base",
      &session_id,
    ) {
      Ok(()) => {
        base.finish_ok(None);
        None
      }
      Err(e) => {
        base.finish_fail(failure::reason::BUILD_FAILED, Some(&e.0));
        Some(e)
      }
    }
  } else {
    None
  };

  if build_failed.is_none() {
    // Same parallel idiom as a run's image builds: the harness images depend only on the
    // (now fresh) base, so one thread each. All builds run to completion; the first
    // failure is the one reported.
    build_failed = std::thread::scope(|scope| {
      let session_ref = session_id.as_str();
      let handles: Vec<_> = harness_build_procs
        .iter()
        .zip(harness_builds.iter())
        .map(|(build, spec)| {
          let rt_name = &rt_name;
          let df = &df;
          let daemon_client = &daemon_client;
          scope.spawn(move || {
            build.start();
            match run_build(
              build,
              rt_name,
              &spec.tag,
              &spec.target,
              df,
              uid,
              gid,
              &spec.fingerprint,
              force,
              daemon_client.as_deref(),
              spec.harness.as_str(),
              session_ref,
            ) {
              Ok(()) => {
                build.finish_ok(None);
                None
              }
              Err(e) => {
                build.finish_fail(failure::reason::BUILD_FAILED, Some(&e.0));
                Some(e)
              }
            }
          })
        })
        .collect();
      handles
        .into_iter()
        .filter_map(|h| h.join().unwrap_or_else(|_| Some(("image build thread panicked".to_string(), 1))))
        .next()
    });
  }
  ui.finish();
  if let Some((msg, code)) = build_failed {
    fail(&msg);
    return code;
  }
  let built = harness_builds.len() + usize::from(build_base);
  ok(&format!("built {built} image{}", plural(built)));
  0
}

#[allow(clippy::too_many_arguments)]
fn print_build_command(
  runtime_name: &str, tag: &str, target: &str, _dockerfile: &str, uid: u32, gid: u32, tz: &str, fingerprint: &str,
) {
  match runtime::build_method(runtime_name) {
    runtime::BuildMethod::Stdin => {
      let build = runtime::build_command_stdin(runtime_name, tag, target, uid, gid, tz, fingerprint, false);
      println!("{}", runtime::shell_join(&build));
    }
    runtime::BuildMethod::ContextDir => {
      let ctx = std::env::temp_dir().join("scsh-build-XXXXXX");
      let build = runtime::build_command_context(
        runtime_name,
        tag,
        target,
        &ctx.to_string_lossy(),
        uid,
        gid,
        tz,
        fingerprint,
        false,
      );
      println!("{}", runtime::shell_join(&build));
      println!("{}", h_dim("# in-memory Dockerfile written to an ephemeral context dir"));
    }
  }
}

fn init_demo() -> i32 {
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return 1;
  }
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return 1;
    }
  };
  let path = root.join(".scsh.yml");
  if path.exists() {
    fail(&format!(".scsh.yml already exists at {} — not overwriting", path.display()));
    hint("delete it first if you want a fresh demo config");
    return 1;
  }
  if let Err(e) = std::fs::write(&path, config::demo_yaml()) {
    fail(&format!("could not write {}: {e}", path.display()));
    return 1;
  }
  ok(&format!("wrote demo config to {}", path.display()));

  // Leave the repo runnable right away: a real `scsh` run refuses to proceed unless
  // the repo's /tmp is gitignored (build scratch and result copies must stay
  // untracked). Set that up now so the next `scsh` clears the guard instead of
  // bouncing off it.
  match ensure_tmp_gitignored(&root) {
    Ok(true) => ok("added '/tmp' to .gitignore (keeps build scratch and result copies untracked)"),
    Ok(false) => {} // already ignored — nothing to change
    Err(e) => hint(&format!("could not update .gitignore automatically ({e}); add a '/tmp' line yourself")),
  }

  // Scaffold the example skills so the demo repo has something real to run. Never
  // overwrite an existing skill file. Track what we wrote so it can be committed.
  let mut skill_paths: Vec<String> = Vec::new();
  for (rel, body, executable) in config::demo_skills() {
    let dest = root.join(rel);
    if dest.exists() {
      hint(&format!("kept existing {rel} (not overwritten)"));
      continue;
    }
    if let Some(parent) = dest.parent() {
      if let Err(e) = std::fs::create_dir_all(parent) {
        hint(&format!("could not create {}: {e}", parent.display()));
        continue;
      }
    }
    match std::fs::write(&dest, body) {
      Ok(()) => {
        // Scripts a skill ships are run directly by the harness, so make them executable.
        #[cfg(unix)]
        if executable {
          use std::os::unix::fs::PermissionsExt;
          let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
        }
        skill_paths.push(rel.to_string());
      }
      Err(e) => hint(&format!("could not write {}: {e}", dest.display())),
    }
  }
  if !skill_paths.is_empty() {
    let n = skill_paths.len();
    ok(&format!("scaffolded {n} example-skill file{} under .skills/", if n == 1 { "" } else { "s" }));
  }

  // Wire up skill discovery the way this repo's convention does (see
  // .skills/README.md): a harness looks for project skills in its OWN dir — none of
  // them know about `.skills/` — so scsh keeps the skills in `.skills/` and symlinks
  // each harness dir to it. That's what lets the opencode harness (and any other)
  // find them; committed with the project, so the links survive the clone scsh
  // mounts into the container.
  let links = link_skill_hosts(&root);
  if !links.is_empty() {
    let n = links.len();
    ok(&format!(
      "linked {n} harness skill dir{} → .skills (so the harness finds the skills)",
      if n == 1 { "" } else { "s" }
    ));
  }

  // Initialize the project *fully*: commit the scaffold so the working tree is clean
  // and the very next `scsh` runs (a real run clones COMMITTED state and refuses a
  // dirty tree). Stage only what we created — never `git add -A` — so any unrelated
  // work already in the repo is left untouched.
  let mut staged = vec![".scsh.yml".to_string()];
  if root.join(".gitignore").exists() {
    staged.push(".gitignore".to_string());
  }
  staged.extend(skill_paths);
  staged.extend(links);
  match commit_scaffold(&root, &staged, "Add scsh demo project (config + skills)") {
    Ok(()) => {
      ok("committed the scaffold");
      let remaining = uncommitted_changes(&root);
      if remaining.is_empty() {
        println!("\nThe project is committed and clean. Next:");
        println!("  {}   {}", bold("scsh run"), h_dim("#  build the image and run the .scsh.yml skills in parallel"));
      } else {
        // We committed the scaffold, but the repo had other uncommitted changes; a
        // real run needs a fully clean tree, so point those out too.
        fail("the repo still has uncommitted changes — a real run needs a clean working tree");
        hint(&format!("commit or stash them, then run {}:", bold("scsh")));
        hint(&format!("{}", bold("git add -A && git commit -m \"wip\"")));
      }
    }
    Err(e) => {
      hint(&format!("couldn't commit the scaffold automatically ({e})"));
      println!("\nNext: commit the scaffold, then run 'scsh' (a run clones committed state):");
      println!("  {}", bold("git add -A && git commit -m \"add scsh demo project\""));
      println!("  {}   {}", bold("scsh run"), h_dim("#  build the image and run the .scsh.yml skills in parallel"));
    }
  }
  print_skill_usage();
  0
}

/// Commit the freshly-scaffolded project so the working tree is clean and the very
/// next `scsh` can run (a real run clones COMMITTED state). Stages only `paths`
/// (never `git add -A`), so unrelated work already in the repo is left untouched.
/// `Err` carries git's message when nothing can be committed or git refuses (e.g.
/// no `user.name`/`user.email` configured) — init then tells the user to commit.
fn commit_scaffold(root: &Path, paths: &[String], message: &str) -> Result<(), String> {
  let add = git_command().arg("-C").arg(root).arg("add").arg("--").args(paths).output();
  let add = add.map_err(|e| format!("git add: {e}"))?;
  if !add.status.success() {
    return Err(String::from_utf8_lossy(&add.stderr).trim().to_string());
  }
  let out = git_command()
    .arg("-C")
    .arg(root)
    .args(["commit", "-q", "-m", message])
    .output()
    .map_err(|e| format!("git commit: {e}"))?;
  if out.status.success() {
    Ok(())
  } else {
    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
  }
}

/// Per-file outcome of an install: how many skill files were newly written, replaced (by
/// `updateskills`), already identical, or kept because they differ from the source.
#[derive(Default)]
struct InstallCounts {
  installed: u32,
  updated: u32,
  already: u32,
  /// Repo-relative paths kept untouched because they differ from the source.
  differing: Vec<String>,
}

impl InstallCounts {
  /// Fold another install's tallies into this one — so installing several source repos in one
  /// command reports a single combined summary.
  fn merge(&mut self, other: InstallCounts) {
    self.installed += other.installed;
    self.updated += other.updated;
    self.already += other.already;
    self.differing.extend(other.differing);
  }
}

/// Install skills into the current repo's `.skills/` plus the harness discovery symlinks.
/// With no `sources`, installs scsh's own bundled skill (see [`config::bundled_skills`]); with
/// one or more `sources` (git URLs or local paths), clones each and installs the
/// `.skills/<name>/` skills it ships, in order — as if the command were run once per repo.
/// `overwrite` (the `updateskills` command) replaces existing files; otherwise an identical
/// file is "already installed", and a differing one is kept untouched. Like a real run, this
/// requires a clean working tree (so the install is a reviewable diff) and ensures `/tmp` is
/// gitignored before writing anything.
fn install_skills(overwrite: bool, sources: &[String], global: bool) -> i32 {
  if global {
    return install_skills_global(overwrite, sources);
  }
  if runtime::which("git").is_none() {
    fail("git is not installed or not on PATH");
    hint(install_git_hint());
    return 1;
  }
  let root = match git_root() {
    Ok(r) => r,
    Err(_) => {
      fail("not inside a git repository");
      hint(&format!("create one here with: {}", bold("git init .")));
      return 1;
    }
  };

  // Install into a clean tree (like a real run), so the install lands as ONE reviewable diff —
  // never silently mixed into unrelated uncommitted work. With several source repos this is
  // checked once, up front, before any of them are installed.
  let dirty = uncommitted_changes(&root);
  if !dirty.is_empty() {
    fail(
      "working tree has uncommitted changes — commit or stash them so the install lands as a clean, reviewable diff",
    );
    let shown = dirty.len().min(10);
    for p in &dirty[..shown] {
      hint(&format!("uncommitted: {p}"));
    }
    if dirty.len() > shown {
      hint(&format!("\u{2026}and {} more", dirty.len() - shown));
    }
    hint(&format!("commit or stash them first, then re-run:  {}", bold("git add -A && git commit -m \"wip\"")));
    return 1;
  }
  if reject_repo_skill_host_copies(&root) {
    return 1;
  }
  // Make the repo run-ready: installed skills write their result + cache under the repo's tmp/,
  // so ensure it's gitignored (append-only, exactly as init-demo-project does).
  match ensure_tmp_gitignored(&root) {
    Ok(true) => ok("added '/tmp' to .gitignore (keeps skill results + cache untracked)"),
    Ok(false) => {}
    Err(e) => hint(&format!("could not update .gitignore automatically ({e}); add a '/tmp' line yourself")),
  }

  // No source → scsh's bundled skill; otherwise install each source repo in order, accumulating
  // the per-file tallies so the final summary covers the whole command.
  let mut counts = InstallCounts::default();
  if sources.is_empty() {
    counts.merge(install_bundled(&root, overwrite));
  } else {
    for url in sources {
      match install_from_repo(&root, overwrite, url) {
        Ok(c) => counts.merge(c),
        Err(code) => return code,
      }
    }
  }

  let any = report_install_counts(&counts, sources, false);

  // Wire up the harness discovery dirs (.opencode/.claude/.cursor/.agents/.codex →
  // ../.skills), exactly as --init-demo-project does; existing ones are left alone.
  let links = link_skill_hosts(&root);
  if !links.is_empty() {
    ok(&format!("linked {} harness skill dir{} → .skills", links.len(), if links.len() == 1 { "" } else { "s" }));
  }
  if !any && links.is_empty() {
    ok("skills already installed; nothing to do");
  }
  // The no-URL install is the reviewer fleet and the original harness demo/self-test —
  // deliberately nothing more: the delivery-pipeline skill families live in their own
  // repositories and install from source, so the bundle can never drift from them.
  if sources.is_empty() {
    hint("installed all five code-review reviewer specialties");
    hint(
      "delivery-pipeline skills install from source: scsh installskills https://github.com/dkorolev/beautiful-skills",
    );
    hint("run /scsh-harness-demo-and-selftest for the basic harness demo, or `scsh run --def gorgeous-pipeline` for the review loop");
  }
  0
}

/// Print the per-file install tallies and the `updateskills` suggestion for kept files.
/// Returns true when any file was installed, updated, already present, or kept.
fn report_install_counts(counts: &InstallCounts, sources: &[String], global: bool) -> bool {
  let InstallCounts { installed, updated, already, differing } = counts;
  if *installed > 0 {
    ok(&format!("installed {installed} skill file{} under .skills/", plural(*installed as usize)));
  }
  if *updated > 0 {
    ok(&format!("updated {updated} skill file{}", plural(*updated as usize)));
  }
  if *already > 0 {
    ok(&format!("{already} skill file{} already installed (identical)", plural(*already as usize)));
  }
  for rel in differing {
    hint(&format!("kept your modified {rel} (it differs from the source)"));
  }
  if !differing.is_empty() {
    let mut cmd = String::from("scsh updateskills");
    if global {
      cmd.push_str(" --global");
    }
    if !sources.is_empty() {
      cmd.push(' ');
      cmd.push_str(&sources.join(" "));
    }
    hint(&format!("to replace them with the source's version, run: {}", bold(&cmd)));
  }
  *installed > 0 || *updated > 0 || *already > 0 || !differing.is_empty()
}

/// `installskills --global`: install machine-wide instead of into a repo. Skills land under
/// `$SCSH_HOME/.skills/` (default `~/.scsh/.skills/`) and their profile blocks merge into the
/// GLOBAL manifest `$SCSH_HOME/.scsh.yml` — the one `run`/`list`/`check-profile` fall back to
/// when the current repo's `.scsh.yml` does not declare a requested profile. Each installed
/// skill is then symlinked into the user-level skills dir of every coding agent present on
/// this machine (`~/.claude/skills`, `~/.cursor/skills`, ...), so agents discover the skills
/// in every project. No git repo and no clean tree are required — nothing is written outside
/// `$SCSH_HOME` and the agents' own skills dirs.
fn install_skills_global(overwrite: bool, sources: &[String]) -> i32 {
  let home = runtime::scsh_home();
  if let Err(e) = std::fs::create_dir_all(&home) {
    fail(&format!("could not create {}: {e}", home.display()));
    return 1;
  }
  // git is only needed to clone URL sources; the bundled no-URL install is git-free.
  if !sources.is_empty() && runtime::which("git").is_none() {
    fail("git is not installed or not on PATH (needed to clone the skills source)");
    hint(install_git_hint());
    return 1;
  }

  // Resolve every incoming skill name before writing anything. An agent-local real copy with
  // the same name would shadow the canonical global skill forever: neither install command may
  // claim success in that state. Source repos are cloned once here and retained for the install
  // below, so this preflight does not double the network work.
  let mut incoming: std::collections::BTreeSet<String> = global_skill_names(&home).into_iter().collect();
  if reject_agent_local_skill_copies(&home, &incoming) {
    return 1;
  }
  let mut clones: Vec<(String, PathBuf)> = Vec::new();
  if sources.is_empty() {
    incoming.extend(bundled_skill_names());
  } else {
    for url in sources {
      let clone = match clone_skill_source(url) {
        Ok(path) => path,
        Err(code) => {
          remove_skill_source_clones(&clones);
          return code;
        }
      };
      match installable_skill_names(url, &clone) {
        Ok(names) => incoming.extend(names),
        Err(code) => {
          let _ = std::fs::remove_dir_all(&clone);
          remove_skill_source_clones(&clones);
          return code;
        }
      }
      clones.push((url.clone(), clone));
    }
  }
  if reject_agent_local_skill_copies(&home, &incoming) {
    remove_skill_source_clones(&clones);
    return 1;
  }

  let mut counts = InstallCounts::default();
  if sources.is_empty() {
    counts.merge(install_bundled(&home, overwrite));
  } else {
    for (url, clone) in &clones {
      match install_from_cloned_repo(&home, overwrite, url, clone) {
        Ok(c) => counts.merge(c),
        Err(code) => {
          remove_skill_source_clones(&clones);
          return code;
        }
      }
    }
  }
  remove_skill_source_clones(&clones);

  let any = report_install_counts(&counts, sources, true);
  let links = link_agent_global_skills(&home);
  if !any && links == 0 {
    ok("skills already installed; nothing to do");
  }
  ok(&format!("global install: {} (manifest + .skills/)", display_path(&home)));
  hint("any git repo can now run these profiles — a repo's own .scsh.yml still wins for the profiles it declares");
  if sources.is_empty() {
    hint("installed all five code-review reviewer specialties");
    hint("try it from any repo: scsh check-profile code-review && scsh run code-review");
    hint("delivery-pipeline skills install from source: scsh installskills --global https://github.com/dkorolev/beautiful-skills");
  }
  0
}

/// Names already owned by the canonical global store. Include these in every global-install
/// preflight, not only the names arriving from this invocation: the command must not leave any
/// previously installed skill shadowed by an agent-local real copy.
fn global_skill_names(scsh_home: &Path) -> Vec<String> {
  let mut names: Vec<String> = std::fs::read_dir(scsh_home.join(".skills"))
    .map(|entries| {
      entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("SKILL.md").is_file())
        .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .filter(|name| !name.starts_with(INTERNAL_PREFIX))
        .collect()
    })
    .unwrap_or_default();
  names.sort();
  names
}

/// Names shipped in the no-URL bundle, derived from the same paths used by the installer.
fn bundled_skill_names() -> Vec<String> {
  config::bundled_skills()
    .iter()
    .filter_map(|(rel, _)| Path::new(rel).parent()?.file_name()?.to_str().map(str::to_string))
    .filter(|name| !name.starts_with(INTERNAL_PREFIX))
    .collect()
}

/// Reject real per-skill copies in agent discovery directories. A real agent `skills/` root is
/// valid because it may contain personal skills, but every scsh-owned child must either be
/// absent (and therefore linkable) or already be a symlink. Returning true means the caller must
/// stop before changing the canonical store.
#[cfg(unix)]
fn reject_agent_local_skill_copies(scsh_home: &Path, names: &std::collections::BTreeSet<String>) -> bool {
  const AGENT_DIRS: &[&str] = &[".claude", ".cursor", ".codex", ".opencode", ".agents"];
  let Some(user_home) = std::env::var_os("HOME").filter(|value| !value.is_empty()).map(PathBuf::from) else {
    return false;
  };
  let mut conflicts = Vec::new();
  for agent in AGENT_DIRS {
    let skills_dir = user_home.join(agent).join("skills");
    let Ok(root_meta) = skills_dir.symlink_metadata() else { continue };
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
      continue;
    }
    for name in names {
      let path = skills_dir.join(name);
      let Ok(meta) = path.symlink_metadata() else { continue };
      if !meta.file_type().is_symlink() {
        conflicts.push(path);
      }
    }
  }
  conflicts.sort();
  if conflicts.is_empty() {
    return false;
  }
  fail("agent skill directories contain local copies that shadow canonical global skills");
  for path in &conflicts {
    hint(&format!("local copy: {}", display_path(path)));
  }
  hint(&format!(
    "move or remove each local copy, then rerun; scsh will link the missing paths to {}",
    display_path(&scsh_home.join(".skills"))
  ));
  true
}

#[cfg(not(unix))]
fn reject_agent_local_skill_copies(_scsh_home: &Path, _names: &std::collections::BTreeSet<String>) -> bool {
  false
}

/// A repository has one canonical `.skills/` tree; every harness discovery path must therefore
/// be absent or a symlink. Refuse before touching `.gitignore`, `.skills/`, or `.scsh.yml` when a
/// real path would prevent that invariant.
#[cfg(unix)]
fn reject_repo_skill_host_copies(root: &Path) -> bool {
  const HOSTS: &[&str] = &[".opencode/skills", ".claude/skills", ".cursor/skills", ".agents/skills", ".codex/skills"];
  let mut conflicts = Vec::new();
  for host in HOSTS {
    let path = root.join(host);
    let Ok(meta) = path.symlink_metadata() else { continue };
    if !meta.file_type().is_symlink() {
      conflicts.push(path);
    }
  }
  conflicts.sort();
  if conflicts.is_empty() {
    return false;
  }
  fail("agent skill discovery paths contain local copies instead of symlinks to .skills");
  for path in &conflicts {
    hint(&format!("local path: {}", display_path(path)));
  }
  hint("move or remove each local path, then rerun; scsh will link the missing paths to .skills");
  true
}

#[cfg(not(unix))]
fn reject_repo_skill_host_copies(_root: &Path) -> bool {
  false
}

/// Symlink every globally-installed skill (`$SCSH_HOME/.skills/<name>`, minus `internal-*`)
/// into every coding agent's user-level skill-discovery dir, mirroring the repo convention
/// ([`link_skill_hosts`]) machine-wide: `~/.claude/skills`, `~/.cursor/skills`,
/// `~/.codex/skills`, `~/.opencode/skills`, and the cross-agent `~/.agents/skills` (where
/// modern codex looks) are each created as ONE symlink to the place the skills actually
/// live, `$SCSH_HOME/.skills` — so every skill installed later is discovered with no
/// re-link. All five are created unconditionally: planting an agent's skills dir ahead of
/// the agent costs nothing and means the skills are found the moment the agent arrives. A
/// skills dir that already exists as a REAL directory (the user's own skills live there) is
/// never replaced — the installed skills are linked into it one by one (authoring-only
/// `internal-*` excluded), and existing entries are left untouched. An existing symlink,
/// wherever it points, is respected. Returns how many links it created.
#[cfg(unix)]
fn link_agent_global_skills(scsh_home: &Path) -> usize {
  const AGENT_DIRS: &[&str] = &[".claude", ".cursor", ".codex", ".opencode", ".agents"];
  let Some(user_home) = std::env::var_os("HOME").filter(|s| !s.is_empty()).map(PathBuf::from) else {
    hint("HOME is not set — skipped linking the skills into agent skills dirs");
    return 0;
  };
  let target_dir = scsh_home.join(".skills");
  // The per-skill set, for agents whose skills dir is a real directory we must not replace.
  let mut skills: Vec<(String, PathBuf)> = std::fs::read_dir(&target_dir)
    .map(|entries| {
      entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("SKILL.md").is_file())
        .filter_map(|p| p.file_name().map(|n| (n.to_string_lossy().into_owned(), p.clone())))
        .filter(|(name, _)| !name.starts_with(INTERNAL_PREFIX))
        .collect()
    })
    .unwrap_or_default();
  skills.sort();

  let mut made = 0usize;
  let mut linked_agents: Vec<String> = Vec::new();
  for agent in AGENT_DIRS {
    let skills_dir = user_home.join(agent).join("skills");
    let Ok(meta) = skills_dir.symlink_metadata() else {
      // Nothing there yet: the whole dir becomes one symlink to $SCSH_HOME/.skills.
      let linked = std::fs::create_dir_all(user_home.join(agent)).is_ok()
        && std::os::unix::fs::symlink(&target_dir, &skills_dir).is_ok();
      if linked {
        made += 1;
        linked_agents.push(format!("~/{agent}/skills"));
      } else {
        hint(&format!("could not link {} — skipped", skills_dir.display()));
      }
      continue;
    };
    if meta.file_type().is_symlink() {
      continue; // already a symlink (ours, or the user's own arrangement) — respected as-is
    }
    // A real directory: the user's own skills live here. Link ours in one by one.
    let mut fresh = 0usize;
    for (name, target) in &skills {
      let link = skills_dir.join(name);
      match std::fs::read_link(&link) {
        Ok(existing) if &existing == target => continue, // already ours
        Ok(_) | Err(_) if link.symlink_metadata().is_ok() => {
          hint(&format!("kept the existing {} (not touching it)", display_path(&link)));
          continue;
        }
        _ => {}
      }
      if std::os::unix::fs::symlink(target, &link).is_ok() {
        fresh += 1;
      }
    }
    if fresh > 0 {
      made += fresh;
      linked_agents.push(format!("~/{agent}/skills ({fresh})"));
    }
  }
  if made > 0 {
    ok(&format!("linked agent skills dirs → {}: {}", display_path(&target_dir), linked_agents.join(", ")));
  }
  made
}

#[cfg(not(unix))]
fn link_agent_global_skills(_scsh_home: &Path) -> usize {
  0
}

/// Install scsh's own skills, embedded in the binary at build time.
fn install_bundled(root: &Path, overwrite: bool) -> InstallCounts {
  let mut c = InstallCounts::default();
  for (rel, body) in config::bundled_skills() {
    write_one(&root.join(rel), body.as_bytes(), rel, overwrite, &mut c);
  }
  merge_bundled_manifest(root);
  c
}

/// Merge the bundled skills' profile blocks into a consumer manifest without replacing any
/// existing key. The embedded repository manifest is the route source of truth; skills without
/// a block (the harness demo/self-test) remain host-invoked skills only.
fn merge_bundled_manifest(root: &Path) {
  let source = config::bundled_skills_manifest();
  let local_path = root.join(".scsh.yml");
  let local_text = std::fs::read_to_string(&local_path).unwrap_or_default();
  let existing: std::collections::BTreeSet<String> = config::validate(&local_text)
    .map(|cfg| cfg.skills.into_iter().map(|skill| skill.name).collect())
    .unwrap_or_default();
  let mut append = String::new();
  let mut added = Vec::new();
  let mut conflicts = Vec::new();
  for (rel, _) in config::bundled_skills() {
    let Some(name) = Path::new(rel).parent().and_then(Path::file_name).and_then(|name| name.to_str()) else {
      continue;
    };
    let Some(block) = config::extract_skill_block(source, name) else { continue };
    if existing.contains(name) {
      conflicts.push(name.to_string());
    } else {
      append.push_str(&block);
      added.push(name.to_string());
    }
  }
  for name in &conflicts {
    hint(&format!("kept your existing '{name}' entry in .scsh.yml (conflicts with bundled manifest)"));
  }
  if append.is_empty() {
    return;
  }
  let merged = if local_text.trim().is_empty() {
    format!("{CONSUMER_MANIFEST_HEADER}{append}")
  } else {
    let mut text = local_text;
    if !text.ends_with('\n') {
      text.push('\n');
    }
    text.push_str(&append);
    text
  };
  if config::validate(&merged).is_ok() && write_file(&local_path, merged.as_bytes()) {
    ok(&format!("added {} bundled skill{} to .scsh.yml: {}", added.len(), plural(added.len()), added.join(", ")));
  } else {
    hint(
      "installed the bundled skill files, but merging their profiles would make .scsh.yml invalid — left it unchanged",
    );
  }
}

/// Header for a `.scsh.yml` that `installskills` creates from scratch in a consumer repo
/// (when it has none yet). The merged skill entries follow the `skills:` line.
const CONSUMER_MANIFEST_HEADER: &str = "\
# .scsh.yml — Scoped Skills Helper. Skills below were added by `scsh installskills`.
# The whole file is just your skills; scsh builds them on a built-in base image.
# Run `scsh help .scsh.yml` for the schema, or `scsh help` for commands.
skills:
";

/// Skills whose name begins with this prefix are authoring-only by convention: scsh never
/// installs them into a consumer repo — the same effect as `autoinstall: false`, but
/// self-evident in the name. Used for a repo's own meta/self-check skills.
const INTERNAL_PREFIX: &str = "internal-";

/// Clone `url` (shallow) and install its skills. If the source ships a `.scsh.yml`, that
/// manifest drives the install (only its listed skills, minus the authoring-only ones —
/// `autoinstall: false` or named `internal-*` — are installed, and each newly-installed
/// skill's entry is merged into the consumer's own `.scsh.yml`); otherwise every
/// `.skills/<name>/` directory is installed (still skipping `internal-*`). Returns
/// `Err(code)` on a clone failure, an invalid source manifest, or no installable skills.
fn install_from_repo(root: &Path, overwrite: bool, url: &str) -> Result<InstallCounts, i32> {
  let clone = clone_skill_source(url)?;
  let result = install_from_cloned_repo(root, overwrite, url, &clone);
  let _ = std::fs::remove_dir_all(&clone);
  result
}

/// Clone one source repo shallowly into a unique scratch directory. The caller owns cleanup.
fn clone_skill_source(url: &str) -> Result<PathBuf, i32> {
  let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
  let clone = std::env::temp_dir().join(format!("scsh-installskills-{}-{nanos}", std::process::id()));
  let _ = std::fs::remove_dir_all(&clone); // clear any stale dir from a crashed run
  let cloned = git_command()
    .args(["clone", "--depth", "1", url])
    .arg(&clone)
    .output()
    .map(|o| o.status.success())
    .unwrap_or(false);
  if !cloned {
    fail(&format!("could not clone {url}"));
    hint("check the URL and your network/credentials, then try again");
    let _ = std::fs::remove_dir_all(&clone);
    return Err(1);
  }
  Ok(clone)
}

/// Install from an already-cloned source repo.
fn install_from_cloned_repo(root: &Path, overwrite: bool, url: &str, clone: &Path) -> Result<InstallCounts, i32> {
  let manifest = clone.join(".scsh.yml");
  if manifest.is_file() {
    install_from_manifest(root, overwrite, url, clone, &manifest)
  } else {
    install_all_skill_dirs(root, overwrite, url, clone)
  }
}

/// Return the installable skill names from a cloned source without modifying the destination.
/// This mirrors the manifest/no-manifest shipping rules used by the installer.
fn installable_skill_names(url: &str, clone: &Path) -> Result<Vec<String>, i32> {
  let manifest = clone.join(".scsh.yml");
  let mut names = Vec::new();
  if manifest.is_file() {
    let src_text = match std::fs::read_to_string(&manifest) {
      Ok(text) => text,
      Err(error) => {
        fail(&format!("{url}: could not read its .scsh.yml: {error}"));
        return Err(1);
      }
    };
    let cfg = match config::validate(&src_text) {
      Ok(config) => config,
      Err(errors) => {
        fail(&format!(
          "{url}: its .scsh.yml does not match the schema ({} problem{})",
          errors.len(),
          plural(errors.len())
        ));
        for error in &errors {
          hint(error);
        }
        return Err(1);
      }
    };
    for skill in cfg.skills {
      if skill.autoinstall
        && !skill.name.starts_with(INTERNAL_PREFIX)
        && clone.join(".skills").join(&skill.name).join("SKILL.md").is_file()
      {
        names.push(skill.name);
      }
    }
  } else if let Ok(entries) = std::fs::read_dir(clone.join(".skills")) {
    names.extend(
      entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("SKILL.md").is_file())
        .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
        .filter(|name| !name.starts_with(INTERNAL_PREFIX)),
    );
  }
  names.sort();
  names.dedup();
  if names.is_empty() {
    fail(&format!("{url}: no installable skills found"));
    return Err(1);
  }
  Ok(names)
}

fn remove_skill_source_clones(clones: &[(String, PathBuf)]) {
  for (_, clone) in clones {
    let _ = std::fs::remove_dir_all(clone);
  }
}

/// Install every `.skills/<name>/` directory in the clone — the behavior when the source
/// ships no `.scsh.yml`. No manifest entries are merged (there is no manifest to read).
fn install_all_skill_dirs(root: &Path, overwrite: bool, url: &str, clone: &Path) -> Result<InstallCounts, i32> {
  let mut c = InstallCounts::default();
  let mut names: Vec<String> = Vec::new();
  let mut skipped: Vec<String> = Vec::new();
  if let Ok(entries) = std::fs::read_dir(clone.join(".skills")) {
    // A skill is a `.skills/<name>/` directory containing a SKILL.md.
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect();
    dirs.sort();
    for dir in dirs {
      let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
      if name.starts_with(INTERNAL_PREFIX) {
        skipped.push(name); // authoring-only by the `internal-` naming convention
        continue;
      }
      copy_skill_dir(root, &dir, &name, overwrite, &mut c);
      names.push(name);
    }
  }
  if names.is_empty() {
    fail(&format!("no skills found in {url} (expected .skills/<name>/SKILL.md)"));
    return Err(1);
  }
  ok(&format!("from {url}: {} skill{} — {}", names.len(), plural(names.len()), names.join(", ")));
  if !skipped.is_empty() {
    ok(&format!("skipped {} authoring-only (internal-*): {}", skipped.len(), skipped.join(", ")));
  }
  Ok(c)
}

/// Install from a source that ships a `.scsh.yml`: validate it (failing on a bad schema),
/// install every listed skill except those marked `autoinstall: false` (skills not listed
/// at all are skipped — the manifest is the shipping list), and merge each newly-installed
/// skill's entry, verbatim, into the consumer's own `.scsh.yml` so `scsh run` (default
/// skills) and `scsh run --profile <p>` pick them up immediately.
fn install_from_manifest(
  root: &Path, overwrite: bool, url: &str, clone: &Path, manifest: &Path,
) -> Result<InstallCounts, i32> {
  let src_text = match std::fs::read_to_string(manifest) {
    Ok(t) => t,
    Err(e) => {
      fail(&format!("{url}: could not read its .scsh.yml: {e}"));
      return Err(1);
    }
  };
  let cfg = match config::validate(&src_text) {
    Ok(c) => c,
    Err(errs) => {
      fail(&format!("{url}: its .scsh.yml does not match the schema ({} problem{})", errs.len(), plural(errs.len())));
      for e in &errs {
        hint(e);
      }
      return Err(1);
    }
  };

  // The consumer's existing manifest (if any) tells us which skills are already declared.
  // `installskills` preserves those blocks as user-owned conflicts; `updateskills` refreshes
  // them from the source alongside the skill files, which also carries profile migrations.
  let local_path = root.join(".scsh.yml");
  let local_text = std::fs::read_to_string(&local_path).unwrap_or_default();
  let existing: std::collections::BTreeSet<String> =
    config::validate(&local_text).map(|c| c.skills.into_iter().map(|s| s.name).collect()).unwrap_or_default();

  let mut c = InstallCounts::default();
  let mut installed: Vec<String> = Vec::new();
  let mut skipped: Vec<String> = Vec::new();
  let mut added: Vec<String> = Vec::new();
  let mut refreshed: Vec<String> = Vec::new();
  let mut conflicts: Vec<String> = Vec::new();
  let mut append = String::new();
  let mut merged = local_text.clone();
  for skill in &cfg.skills {
    // Authoring-only skills are not installed: either marked `autoinstall: false`, or named
    // with the `internal-` convention (a self-documenting "internal to this repo" marker).
    if !skill.autoinstall || skill.name.starts_with(INTERNAL_PREFIX) {
      skipped.push(skill.name.clone());
      continue;
    }
    let dir = clone.join(".skills").join(&skill.name);
    if !dir.join("SKILL.md").is_file() {
      hint(&format!("{}: listed in .scsh.yml but has no .skills/{}/SKILL.md — skipped", skill.name, skill.name));
      continue;
    }
    copy_skill_dir(root, &dir, &skill.name, overwrite, &mut c);
    if !installed.contains(&skill.name) {
      installed.push(skill.name.clone());
    }
    if existing.contains(&skill.name) {
      if overwrite {
        if let Some(block) = config::extract_skill_block(&src_text, &skill.name) {
          if let Some(replaced) = config::replace_skill_block(&merged, &skill.name, &block) {
            if replaced != merged {
              merged = replaced;
              refreshed.push(skill.name.clone());
            }
          }
        }
      } else {
        conflicts.push(skill.name.clone());
      }
      continue;
    }
    if let Some(block) = config::extract_skill_block(&src_text, &skill.name) {
      append.push_str(&block);
      added.push(skill.name.clone());
    }
  }

  if installed.is_empty() {
    fail(&format!("{url}: its .scsh.yml lists no installable skills (all authoring-only or missing)"));
    return Err(1);
  }
  ok(&format!("from {url}: {} skill{} — {}", installed.len(), plural(installed.len()), installed.join(", ")));
  if !skipped.is_empty() {
    ok(&format!("skipped {} authoring-only (autoinstall: false or internal-*): {}", skipped.len(), skipped.join(", ")));
  }
  if !conflicts.is_empty() {
    for name in &conflicts {
      hint(&format!("kept your existing '{name}' entry in .scsh.yml (conflicts with source manifest)"));
    }
  }

  // Merge new entries and any explicitly refreshed entries into the consumer manifest, but
  // only if the complete result still validates.
  if !append.is_empty() {
    merged = if merged.trim().is_empty() {
      format!("{CONSUMER_MANIFEST_HEADER}{append}")
    } else {
      if !merged.ends_with('\n') {
        merged.push('\n');
      }
      merged.push_str(&append);
      merged
    };
  }
  if merged == local_text {
    if conflicts.is_empty() && refreshed.is_empty() {
      ok("the installed skills were already declared in .scsh.yml");
    }
  } else {
    if config::validate(&merged).is_ok() && write_file(&local_path, merged.as_bytes()) {
      if !refreshed.is_empty() {
        ok(&format!(
          "updated {} skill declaration{} in .scsh.yml: {}",
          refreshed.len(),
          plural(refreshed.len()),
          refreshed.join(", ")
        ));
      }
      if !added.is_empty() {
        ok(&format!("added {} skill{} to .scsh.yml: {}", added.len(), plural(added.len()), added.join(", ")));
      }
    } else {
      hint("installed the skill files, but updating .scsh.yml would make it invalid — left the manifest unchanged");
      let affected: Vec<&str> = refreshed.iter().chain(&added).map(String::as_str).collect();
      hint(&format!("update by hand: {}", affected.join(", ")));
    }
  }
  Ok(c)
}

/// Copy one skill directory (every file under it) from `src` into `root/.skills/<name>/`,
/// applying the per-file install rules.
fn copy_skill_dir(root: &Path, src: &Path, name: &str, overwrite: bool, c: &mut InstallCounts) {
  let dest_dir = root.join(".skills").join(name);
  let mut files = Vec::new();
  collect_files(src, src, &mut files);
  files.sort();
  for (rel, abs) in files {
    if let Ok(body) = std::fs::read(&abs) {
      write_one(&dest_dir.join(&rel), &body, &format!(".skills/{name}/{rel}"), overwrite, c);
    }
  }
}

/// Apply the install rules for one file: write if new, replace if `overwrite`, count as
/// already-installed if identical, or keep it (recording `shown`) if it differs.
fn write_one(dest: &Path, body: &[u8], shown: &str, overwrite: bool, c: &mut InstallCounts) {
  if dest.is_file() {
    let same = std::fs::read(dest).map(|d| d == body).unwrap_or(false);
    if same {
      c.already += 1;
    } else if overwrite {
      if write_file(dest, body) {
        c.updated += 1;
      }
    } else {
      c.differing.push(shown.to_string());
    }
  } else if write_file(dest, body) {
    c.installed += 1;
  }
}

/// Write a file atomically (see [`atomic_write`]), creating its parent dir. Reports and
/// returns false on error.
fn write_file(dest: &Path, body: &[u8]) -> bool {
  if let Some(parent) = dest.parent() {
    if let Err(e) = std::fs::create_dir_all(parent) {
      hint(&format!("could not create {}: {e}", parent.display()));
      return false;
    }
  }
  match atomic_write(dest, body) {
    Ok(()) => true,
    Err(e) => {
      hint(&format!("could not write {}: {e}", dest.display()));
      false
    }
  }
}

/// Symlink each harness's project skill-discovery dir at this repo's `.skills/`,
/// following the repo convention (see `.skills/README.md`): a harness reads skills
/// from its own dir — `.opencode/skills`, `.claude/skills`, `.cursor/skills`,
/// `.agents/skills`, `.codex/skills` — and none know about `.skills/`, so each is a
/// relative symlink (`../.skills`) to the one place the skills actually live. An
/// existing path (real dir or symlink) is left untouched. Returns how many it made.
#[cfg(unix)]
fn link_skill_hosts(root: &Path) -> Vec<String> {
  const HOSTS: &[&str] = &[".opencode/skills", ".claude/skills", ".cursor/skills", ".agents/skills", ".codex/skills"];
  let mut made = Vec::new();
  for host in HOSTS {
    let link = root.join(host);
    if link.symlink_metadata().is_ok() {
      continue; // already present — leave it
    }
    let linked = link.parent().map(|p| std::fs::create_dir_all(p).is_ok()).unwrap_or(false)
      && std::os::unix::fs::symlink("../.skills", &link).is_ok();
    if linked {
      made.push((*host).to_string());
    }
  }
  made
}

#[cfg(not(unix))]
fn link_skill_hosts(_root: &Path) -> Vec<String> {
  Vec::new()
}

/// Show how to run the scaffolded example skills.
fn print_skill_usage() {
  println!("\nThe demo .scsh.yml runs `add` on two routes by default (opencode+GPT, claude+Sonnet),");
  println!("plus `subtract` (C - D) — a second commit-enabled step, so one run brings in two");
  println!("commits from two different steps. `multiply` (X * Y) lives in the `multiply` profile");
  println!("because it REQUIRES X and Y. `demo-pr` is the multi-agent fake-PR skill; the built-in");
  println!("`smoke-pr-claude` / `smoke-pr-codex` / `smoke-pr-grok` / `smoke-pr-cursor` defs smoke one");
  println!("harness at a time (feature note + PR-DESCRIPTION.md). scsh resolves the env you");
  println!(
    "forward (or refuses the skill). Examples — successes ({}) and the intended refusal ({}):",
    ok_mark(),
    refused_mark()
  );
  println!();
  example("scsh run", "add 2 + 3 = 5, subtract 10 - 4 = 6 (both commit)", true);
  example("A=10 B=20 scsh run", "add forwards your A,B -> 10 + 20 = 30", true);
  example("X=6 Y=7 scsh run --profile multiply", "also runs multiply -> 6 * 7 = 42", true);
  example("scsh run --profile multiply", "multiply REFUSED — X is required by ${X}", false);
  example("scsh run demo-pr-claude-sonnet", "fake PR on claude (⇄ commits diff + Description)", true);
  example("scsh run --def smoke-pr-claude", "builtin smoke fake-PR on Claude only", true);
  println!();
  let (var, def, req) = (env_syntax("${VAR}"), env_syntax("${VAR:-default}"), env_syntax("${VAR:?msg}"));
  println!("The env syntax: {var} requires VAR, {def} injects a default, {req}");
  println!("requires it with your message, and a bare literal is just that literal.");
  println!("When a skill finishes, scsh prints the message from its JSON result file (e.g.");
  println!("\"6 * 7 = 42\"), not just the file path. Preview the resolved env without containers:");
  println!("  {}      (shows every skill and the profile that runs it).", bold("scsh list"));
  println!(
    "Or from any clean repo (no scaffold): {} / {} / {} / {}.",
    bold("scsh run --def smoke-pr-claude"),
    bold("smoke-pr-codex"),
    bold("smoke-pr-grok"),
    bold("smoke-pr-cursor")
  );
}

/// An env-syntax token (e.g. `${VAR}`), in cyan to set it apart from the prose.
fn env_syntax(token: &str) -> console::StyledObject<&str> {
  console::style(token).cyan()
}

/// A green ✓ for an example that works.
fn ok_mark() -> console::StyledObject<&'static str> {
  console::style("\u{2713}").green()
}

/// A grey ✗ for an example scsh intentionally refuses — it's the expected guardrail, not an
/// error, so it reads dim rather than alarming red.
fn refused_mark() -> console::StyledObject<&'static str> {
  console::style("\u{2717}").dim()
}

/// One example line: `  <command>  #  <comment>  <✓|✗>` — the command bold, the comment dimmed
/// (its `${…}` tokens cyan), and the mark green for a success or grey for an intended refusal.
fn example(cmd: &str, comment: &str, ok: bool) {
  let mark = if ok { ok_mark() } else { refused_mark() };
  let cmd = console::style(format!("{cmd:<35}")).bold();
  println!("  {cmd} {}  {mark}", dim_comment(comment));
}

/// Render a `#  <comment>` for an example line: dimmed, but with any `${…}` token in cyan so the
/// env syntax stands out even inside the comment.
fn dim_comment(comment: &str) -> String {
  let mut out = format!("{}", h_dim("#  "));
  let mut rest = comment;
  while let Some(start) = rest.find("${") {
    if let Some(end) = rest[start..].find('}') {
      let end = start + end + 1; // include the '}'
      out.push_str(&format!("{}", h_dim(&rest[..start])));
      out.push_str(&format!("{}", env_syntax(&rest[start..end])));
      rest = &rest[end..];
      continue;
    }
    break;
  }
  out.push_str(&format!("{}", h_dim(rest)));
  out
}

/// Ensure the repo ignores its `/tmp` (repo-root) path, appending a `/tmp` rule to
/// `<root>/.gitignore` when it isn't already ignored (creating the file if needed),
/// and create the physical `tmp/` directory so results/logs/cache have a home.
/// Returns whether a gitignore rule was added (`false` = already ignored).
/// It only ever **appends** — existing `.gitignore` content is never rewritten.
fn ensure_tmp_gitignored(root: &std::path::Path) -> Result<bool, String> {
  let added = if tmp_is_gitignored(root) {
    false
  } else {
    let path = root.join(".gitignore");
    let mut content = match std::fs::read_to_string(&path) {
      Ok(s) => s,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
      Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    if !content.is_empty() && !content.ends_with('\n') {
      content.push('\n');
    }
    content.push_str("# scsh uses the system temp dir for build scratch; never track a local /tmp.\n/tmp\n");
    atomic_write(&path, content.as_bytes()).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    true
  };
  std::fs::create_dir_all(root.join("tmp")).map_err(|e| format!("could not create {}/tmp: {e}", root.display()))?;
  Ok(added)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write `bytes` to `path` atomically: the data first lands in a sibling temporary file
/// in the same directory (a sibling keeps the final rename on one filesystem), is synced
/// to disk, and only then renamed over the destination. A failed or interrupted write
/// therefore leaves the previous complete file in place instead of a truncated one, and
/// the temporary file is removed on failure. Every durable artifact — the user's
/// `.gitignore` and `.scsh.yml`, cast sidecars and exports, daemon state files — routes
/// through here rather than a plain truncate-write.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  let mut tmp_name = path.as_os_str().to_owned();
  tmp_name.push(format!(".tmp.{}", std::process::id()));
  let tmp = PathBuf::from(tmp_name);
  let written = (|| {
    use std::io::Write;
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    // Keep the destination's permissions (say, a user's own .gitignore) across the rename.
    if let Ok(meta) = std::fs::metadata(path) {
      std::fs::set_permissions(&tmp, meta.permissions())?;
    }
    std::fs::rename(&tmp, path)
  })();
  if written.is_err() {
    let _ = std::fs::remove_file(&tmp);
  }
  written
}

fn git_root() -> Result<PathBuf, String> {
  let out =
    git_command().args(["rev-parse", "--show-toplevel"]).output().map_err(|e| format!("failed to run git: {e}"))?;
  if !out.status.success() {
    return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
  }
  Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// The git top-level of an arbitrary directory (not the process cwd), or `None` when `dir`
/// is not inside a git work tree. Shared with the daemon's "open a repository" validation
/// (a bare `uncommitted_changes` reports a non-repo as clean, so callers resolve the root first).
fn git_root_of(dir: &Path) -> Option<PathBuf> {
  let path = git_capture(dir, &["rev-parse", "--show-toplevel"])?;
  let path = path.trim();
  (!path.is_empty()).then(|| PathBuf::from(path))
}

fn ok(msg: &str) {
  println!("{} {msg}", console::style("\u{2713}").green().bold());
}

fn fail(msg: &str) {
  eprintln!("{} {msg}", console::style("\u{2717}").red().bold().for_stderr());
}

fn hint(msg: &str) {
  eprintln!("  {} {msg}", console::style("\u{2192}").cyan().for_stderr());
}

/// A warning that isn't a hard failure — e.g. a skill's commits were saved to a branch
/// because they couldn't be rebased cleanly. Yellow `⚠`, so it stands apart from ✓/✗.
fn warn(msg: &str) {
  eprintln!("{} {msg}", console::style("\u{26a0}").yellow().bold().for_stderr());
}

/// A literal command rendered **bold** for an actionable hint, so the thing to type
/// stands out. Honors NO_COLOR and non-TTY output via `console` (plain text then).
fn bold(s: &str) -> console::StyledObject<&str> {
  console::style(s).bold().for_stderr()
}

fn install_git_hint() -> &'static str {
  if cfg!(target_os = "macos") {
    "install it with: brew install git  (or: xcode-select --install)"
  } else {
    "install it with your package manager, e.g.: sudo apt-get install git"
  }
}

fn install_runtime_hint() -> &'static str {
  if cfg!(target_os = "macos") {
    "install Apple 'container' (https://github.com/apple/container), Docker Desktop, or Podman"
  } else {
    "install Docker (https://docs.docker.com/engine/install/) or Podman (https://podman.io)"
  }
}

// --- help styling -----------------------------------------------------------
// All help goes to stdout; `console` auto-strips color when piped or NO_COLOR is
// set, so the text stays the same and tests still match on plain substrings.

/// A bold, cyan section header (`Commands:`, `Usage:`, …) — the "dark color" accent.
fn h_head(s: &str) -> console::StyledObject<&str> {
  console::style(s).cyan().bold()
}

/// Dimmed secondary text (taglines, descriptions, the aliases footer).
fn h_dim(s: &str) -> console::StyledObject<&str> {
  console::style(s).dim()
}

/// One `• name    description` row: a dim bullet, a bold fixed-width name, dim text.
fn help_row(name: &str, desc: &str) {
  println!("  {} {} {}", h_dim("\u{2022}"), console::style(format!("{name:<25}")).bold(), h_dim(desc));
}

/// Indented continuation line for a multi-line help entry (aligns under the description).
fn help_cont(desc: &str) {
  println!("      {}", h_dim(desc));
}

fn print_help(topic: HelpTopic) {
  match topic {
    HelpTopic::Overview => print_help_overview(),
    HelpTopic::Run => print_help_run(),
    HelpTopic::Config => print_help_config(),
    HelpTopic::Internals => print_help_internals(),
    HelpTopic::Cache => print_help_cache(),
    HelpTopic::Agent => print_help_agent(),
    HelpTopic::Defs => print_help_defs(),
    HelpTopic::ExitCodes => print_help_exitcodes(),
    // `run` has a dedicated deep-dive page; every other command gets a focused block.
    HelpTopic::Command(name) if name == "run" => print_help_run(),
    HelpTopic::Command(name) => print_help_command(&name),
  }
}

/// `scsh help exitcodes` — the documented exit-code table (§2). scsh uses only the three
/// conventional codes; commands never define custom ones, so this table is complete.
fn print_help_exitcodes() {
  println!();
  println!("{} {}", h_head("Exit codes"), console::style("\u{2014} the same across every command").bold());
  println!();
  help_row("0", "Success — the command did what was asked.");
  help_row("1", "Failure — a skill failed, a check did not pass, or the operation could not complete.");
  help_row("2", "Usage error — a bad argument, unknown command, or missing required value.");
  println!();
  println!("{}", h_dim("  scsh defines no command-specific exit codes; these three are the whole set."));
  println!();
}

/// The embedded, agent-followable demo walkthroughs (`scsh demo <name>`). Each is the repo's
/// markdown file, printed verbatim to stdout, so a driving agent needs no path to any
/// checkout — `scsh` on PATH is enough.
const DEMOS: &[(&str, &str, &str)] = &[(
  "agent-fleet",
  "One agent fans \"explain this codebase\" out to claude, codex, and cursor; one blocking run.",
  include_str!("../AGENT-FLEET-DEMO.md"),
)];

/// `scsh demo [name]` — with a name, print that walkthrough verbatim (pipe it, save it, or
/// follow it); with none, list what's available. `agent-fleet-demo` and the file spelling
/// `AGENT-FLEET-DEMO.md` resolve to `agent-fleet`.
fn demo_cmd(name: Option<&str>) -> i32 {
  let Some(requested) = name else {
    println!();
    println!(
      "{} {}",
      h_head("Demos"),
      console::style("\u{2014} print one with `scsh demo <name>`, then follow it").bold()
    );
    println!();
    for (demo_name, blurb, _) in DEMOS {
      help_row(demo_name, blurb);
    }
    println!();
    println!("{}", h_dim("  Each demo is a self-contained markdown walkthrough: hand the output to an"));
    println!("{}", h_dim("  agent (or follow it yourself) from any directory not inside a git repo."));
    println!();
    return 0;
  };
  let normalized = requested.to_ascii_lowercase();
  let normalized = normalized.trim_end_matches(".md").trim_end_matches("-demo");
  match DEMOS.iter().find(|(demo_name, _, _)| *demo_name == normalized) {
    Some((_, _, markdown)) => {
      print!("{markdown}");
      0
    }
    None => {
      let names: Vec<&str> = DEMOS.iter().map(|(demo_name, _, _)| *demo_name).collect();
      eprintln!("scsh: no demo named '{requested}' (available: {})", names.join(", "));
      eprintln!("try 'scsh demo'");
      2
    }
  }
}

/// `scsh help agent` — the agent-first contract: how another agent or harness drives scsh
/// end to end. Written to the driving agent in second person; exit codes and JSON only,
/// never the human-formatted output.
fn print_help_agent() {
  println!();
  println!("{} {}", h_head("agent"), console::style("\u{2014} drive scsh from another agent or harness").bold());
  println!();
  println!("{}", h_dim("You are an agent. scsh is your fan-out primitive: one command runs a repo's"));
  println!("{}", h_dim("skills as separate agent jobs in parallel \u{2014} each agent (claude, codex, cursor,"));
  println!("{}", h_dim("opencode, grok) in its own container on a clean clone \u{2014} and BLOCKS until every"));
  println!("{}", h_dim("job has written its result file. You never poll and never scrape a TTY:"));
  println!("{}", h_dim("everything you need is exit codes and JSON."));
  println!();
  println!("{}", h_head("The loop"));
  help_row("1. DISCOVER", "scsh list --json \u{2014} profiles, skills, routes, and each job's result path.");
  help_row("2. GATE", "scsh check-profile <p> \u{2014} exit 0 iff the profile exists (runtime-free).");
  help_cont("scsh probe [p\u{2026}] --json \u{2014} which agent\u{b7}model routes can run on this host.");
  help_row("3. RUN", "scsh run [p\u{2026}] \u{2014} synchronous; exit 0 = every job succeeded. Pass skill");
  help_cont("inputs as environment variables (declared in .scsh.yml `env:`).");
  help_row("4. COLLECT", "Read each job's result file \u{2014} the paths came from step 1, one per route,");
  help_cont("always under the repo's gitignored tmp/.");
  help_row("5. RE-RUN", "Same commit + same env = instant cached result; re-running is free.");
  println!();
  println!("{}", h_head("Bring your own work to any repo"));
  println!("{}", h_dim("  scsh run --override-dot-scsh-yml <bundle>/.scsh.yml"));
  help_cont("Run an external bundle's skills (config + sibling .skills/) against ANY clean");
  help_cont("git repo: the target needs no .scsh.yml and no .skills/, and stays byte-clean \u{2014}");
  help_cont("results land only under its gitignored tmp/. probe/list/check-profile take the");
  help_cont("same flag, so the whole loop above works without installing anything.");
  println!();
  println!("{}", h_head("What scsh enforces (so you don't have to)"));
  help_row("committed state", "A real run insists on a clean tree and runs the COMMIT, never the tree.");
  help_row("route skipping", "Unavailable routes are skipped; the run fails only when ALL are skipped.");
  help_row("preflight", "Missing required env is refused before any container starts.");
  help_row("exit codes", "0 ok \u{b7} 1 failure \u{b7} 2 usage \u{2014} the whole set (scsh help exitcodes).");
  println!();
  println!("{}", h_dim("  Worked end-to-end example: `scsh demo agent-fleet` prints AGENT-FLEET-DEMO.md \u{2014}"));
  println!(
    "{}",
    h_dim("  one agent (you) fans \u{201c}explain this codebase\u{201d} out to claude, codex, and cursor")
  );
  println!("{}", h_dim("  as three agent jobs, waits on one blocking run, then synthesizes the three"));
  println!("{}", h_dim("  JSON results. No checkout path needed \u{2014} follow what the command prints."));
  println!();
}

/// `scsh help <command>` — a focused block for one command: canonical synopsis, what it does,
/// its flags, and a pointer. Descriptions match the overview's one-liners.
fn print_help_command(name: &str) {
  println!();
  let (tagline, synopsis, body): (&str, &str, &[(&str, &str)]) = match name {
    "list" => (
      "list skills by profile",
      "scsh list [--verbose] [--json]",
      &[
        ("--verbose", "Show the full per-skill build/run commands (human-readable)."),
        ("--json", "Machine-readable listing; on by default when stdout is not a TTY."),
      ],
    ),
    "check-profile" => (
      "test whether a profile exists",
      "scsh check-profile <name> [--override-dot-scsh-yml <path>]",
      &[
        ("<name>", "Exit 0 iff that profile exists with >=1 skill; runtime-free (no build)."),
        ("--override-dot-scsh-yml <path>", "Check against this external `.scsh.yml` instead of the repo's."),
      ],
    ),
    "probe" => (
      "which model routes are runnable on this host",
      "scsh probe [profile…] [--json]",
      &[
        (
          "[profile…]",
          "Probe those profiles' harness·model routes (deduped across skills); no profile = every route in the config. Exit 0 when at least one route is available, 1 when none is — gate a fleet with `scsh probe code-review && scsh run code-review`.",
        ),
        ("--json", "Machine-readable routes with per-route availability and the reason when unavailable."),
        ("--override-dot-scsh-yml <path>", "Probe against this external `.scsh.yml` instead of the repo's."),
      ],
    ),
    "init-demo-project" => (
      "scaffold a demo project",
      "scsh init-demo-project",
      &[("(no flags)", "Write and commit a small demo .scsh.yml + skills into the current dir.")],
    ),
    "demo" => (
      "print an embedded agent-followable walkthrough",
      "scsh demo [name]",
      &[
        ("(no name)", "List the available demos."),
        (
          "agent-fleet",
          "One agent fans \"explain this codebase\" out to claude, codex, and cursor as three agent jobs through one blocking `scsh run`, then synthesizes the three JSON results. Prints AGENT-FLEET-DEMO.md verbatim — no checkout path needed.",
        ),
      ],
    ),
    "installskills" => (
      "install skills into this repo (or machine-wide)",
      "scsh installskills [--global] [git-url…]",
      &[
        ("[git-url…]", "Install from the bundled skills, or from one or more source repos in order."),
        (
          "--global",
          "Install under $SCSH_HOME (~/.scsh) instead: skills + a global manifest that run/list/check-profile fall back to when the repo's .scsh.yml lacks the profile, plus symlinks into each present agent's ~/.<agent>/skills. No git repo needed.",
        ),
      ],
    ),
    "updateskills" => (
      "update skills and manifest blocks",
      "scsh updateskills [--global] [git-url…]",
      &[(
        "[git-url…]",
        "Like installskills, but overwrites each skill's local files and existing source manifest block.",
      )],
    ),
    "daemon" => (
      "the session-browser daemon",
      "scsh daemon <start|stop|restart|status>",
      &[
        ("start", "Run a persistent daemon until `daemon stop`."),
        ("stop", "Stop the running daemon."),
        ("restart", "Stop then start (persistent)."),
        ("status", "Exit 0 when the daemon is listening."),
        ("SCSH_DAEMON_PORT", "Listen port (default 7274)."),
        (
          "SCSH_HOME",
          "Dir for the session store and permanent per-session artifacts (sessions/<id>/casts|logs; default ~/.scsh).",
        ),
      ],
    ),
    "failures" => (
      "browse the failure log",
      "scsh failures [--session S] [--skill N] [--reason C] [--last N] [--stats]",
      &[
        ("--session/--skill/--reason", "Filter by session id, skill name, or reason code."),
        ("--last N", "Show only the last N entries (0 = all)."),
        ("--stats", "Aggregate counts per reason instead of listing entries."),
      ],
    ),
    "stats" => (
      "run durations & workload per route",
      "scsh stats [--skill N] [--profile P] [--harness H] [--model M] [--raw] [--last N]",
      &[
        ("--skill/--profile", "Filter by skill name or profile."),
        ("--harness/--model", "Filter by harness or model route."),
        ("--raw", "One row per run instead of aggregates."),
        ("--last N", "Limit to the last N runs."),
      ],
    ),
    "prune" => (
      "the run-dir cleanup queue",
      "scsh prune [--now]",
      &[("--now", "Force a janitor pass now instead of just showing the queue.")],
    ),
    "gc" => (
      "reclaim old session artifact dirs",
      "scsh gc [--dry-run] | scsh gc --apply [--days N] [--keep N] [--legacy]",
      &[
        ("(default)", "Dry-run: list reclaimable paths under $SCSH_HOME/sessions/."),
        ("--dry-run", "Explicit dry-run (same as the default)."),
        ("--apply", "Actually delete candidates (required to free disk)."),
        ("--days N", "Only dirs older than N days by mtime (default 30)."),
        ("--keep N", "Always retain the N newest session dirs (default 50)."),
        ("--legacy", "Also remove top-level $SCSH_HOME/casts/ and recordings/."),
      ],
    ),
    "build-images" => (
      "build the container images outside a run",
      "scsh build-images [harness…] [--force] [--rebuild-base] [--session <id>]",
      &[
        ("[harness…]", "Harness images to build (opencode, claude, codex, grok, cursor); none = all."),
        ("--force", "Rebuild the selected harness images even when up to date (--no-cache)."),
        ("--rebuild-base", "Force-rebuild the shared base image first (--no-cache), then the selection."),
        ("--session <id>", "Report into this session id (used by the dashboard's Build buttons)."),
      ],
    ),
    "annotate-cast" => (
      "summarize + chapter recordings",
      "scsh annotate-cast <cast…> [--json]",
      &[
        ("<cast…>", "One or more .cast files to annotate via Codex on Luna."),
        ("--json", "Emit the written sidecar paths as JSON; on by default when stdout is not a TTY."),
        ("SCSH_ANNOTATE_MODEL", "Override the model (default gpt-5.6-luna)."),
      ],
    ),
    "export-cast" => (
      "render casts to offline HTML pages",
      "scsh export-cast <cast…> [-o <file>] [--json]",
      &[
        ("<cast…>", "One or more .cast files; each renders to <stem>.html next to it."),
        ("-o <file>", "Output path (exactly one cast); `-o -` streams the page to stdout."),
        ("--json", "Per-cast {input, output, bytes, chapters} JSON; on by default when stdout is not a TTY."),
        ("<stem>.chapters.json", "A summary+chapters sidecar next to the cast renders into the page when present;"),
        ("", "a malformed sidecar is a warning — the cast still exports without it."),
        ("(license)", "Exported pages embed the first-party beecast-player — everything is MIT,"),
        ("", "no third-party code or license in any page. See LICENSE.md."),
      ],
    ),
    "version" => {
      ("print the version", "scsh version", &[("(no flags)", "Print the version with the build's git hash.")])
    }
    _ => ("", "scsh help", &[]),
  };
  println!("{} {}", h_head(name), console::style(format!("\u{2014} {tagline}")).bold());
  println!();
  println!("{}", h_head("Synopsis"));
  println!("{}", h_dim(&format!("  {synopsis}")));
  println!();
  if !body.is_empty() {
    println!("{}", h_head("Flags & arguments"));
    for (flag, desc) in body {
      help_row(flag, desc);
    }
    println!();
  }
  println!("{}", h_dim("  See `scsh help` for all commands, `scsh help exitcodes` for exit codes."));
  println!();
}

/// The default page: a compact, one-line-per-command overview. The detail lives in
/// the two deep-dive topics so it never floods this screen.
fn print_help_overview() {
  println!();
  println!(
    "{} {} {}",
    console::style("scsh").cyan().bold(),
    h_dim(&version_id()),
    console::style("\u{2014} Scoped Skills Helper").bold()
  );
  println!("{}", h_dim("Run a git repo's scoped skills in parallel \u{2014} each in its own ephemeral"));
  println!("{}", h_dim("container, on a clean clone of your repo \u{2014} all from one .scsh.yml."));
  println!();
  println!(
    "{} {} {}",
    h_head("Usage:"),
    console::style("scsh <command> [options]").bold(),
    h_dim("\u{2014} a bare `scsh` prints this help")
  );
  println!();
  println!("{}", h_head("Commands:"));
  help_row("run [profile…]", "Build the image; run skills in parallel.");
  help_cont("See `scsh help run` for profiles, preflight, and exit codes.");
  help_row("list (ls)", "List skills by profile (--verbose, --json).");
  help_row("build-images [harness…]", "Build the base + harness images outside a run (--force, --rebuild-base).");
  help_row("check-profile <name>", "Exit 0 when the profile exists and has skills.");
  help_row("probe [profile…]", "Exit 0 when at least one harness·model route is runnable on this host.");
  help_row("init-demo-project", "Scaffold and commit a demo project.");
  help_row("demo [name]", "Print an embedded agent-followable walkthrough (no name lists them).");
  help_row("installskills [url…]", "Install skills (bundled or from git URLs); --global installs machine-wide.");
  help_row("updateskills [url…]", "Reinstall skills, overwriting local copies (--global for the machine-wide set).");
  help_row("daemon", "start | stop | restart | status");
  help_cont("Browse run output at http://127.0.0.1:7274 (override: SCSH_DAEMON_PORT).");
  help_row("failures", "Browse the failure log (--session, --skill, --reason, --last, --stats).");
  help_row("stats", "Durations & workload per skill/route (--skill, --profile, --harness, --model, --raw).");
  help_row("prune [--now]", "Show the run-dir cleanup queue; --now forces a pass.");
  help_row("gc [--apply]", "Reclaim old $SCSH_HOME/sessions/ dirs (dry-run default; --days/--keep/--legacy).");
  help_row("annotate-cast <cast…>", "Summarize + chapter recordings via Codex / Luna (--json).");
  help_row("export-cast <cast…>", "Render recordings to self-contained offline HTML pages (-o, --json).");
  help_row("version", "Print the version (with the build's git hash).");
  help_row("help [topic]", "Show this help, or one of the topics below.");
  println!();
  println!("{}", h_head("More help:"));
  help_row("scsh help agent", "Driving scsh from another agent or harness? Start here.");
  help_row("scsh help <command>", "Focused help for any command above (e.g. `scsh help stats`).");
  help_row("scsh help run", "How to run skills: profiles, preflight, exit codes, env vars.");
  help_row("scsh help .scsh.yml", "The project config file: every field + env syntax.");
  help_row("scsh help internals", "How a run works: clone, containers, auth, live board.");
  help_row("scsh help cache", "How results are cached, and when a re-run is a hit.");
  help_row("scsh help def", "Harness definitions: flat tasks, workflow steps, loops (repeat / do-while).");
  help_row("scsh help exitcodes", "The exit-code table (0 ok, 1 failure, 2 usage).");
  println!();
  println!("{}", h_head("Options:"));
  help_row("--profile <names>", "With `run`: only these profiles (`default` = no-profile skills).");
  help_row("--override-dot-scsh-yml <path>", "With `run`/`list`/`check-profile`: use this `.scsh.yml`");
  help_cont("(+ sibling `.skills/`) instead of the repo's — no install into the target tree.");
  help_row("--verbose", "With list: also print the Dockerfile and exact commands.");
  help_row("--json", "With list: print profiles and skills as JSON.");
  println!();
  println!("{}", h_dim("`run` bakes a dev toolchain into the image (python3/uv, Go, Rust, gh, aws, gcloud,"));
  println!("{}", h_dim("kubectl, psql, protoc, \u{2026}; no Java) and builds it with this machine's timezone."));
  println!("{}", h_dim("Full toolchain list: scsh help internals."));
  println!();
  println!("{} {}", h_dim("Aliases:"), h_dim("--help/-h \u{00b7} --version/-V \u{00b7} --init-demo-project"));
  println!();
}

/// `scsh help run` — how to invoke `run`, for humans and agents.
fn print_help_run() {
  println!();
  println!("{} {}", h_head("run"), console::style("\u{2014} run scoped skills in parallel").bold());
  println!();
  println!("{}", h_head("Synopsis"));
  println!("{}", h_dim("  scsh run [profile…] [--override-dot-scsh-yml <path>] [--base <ref>]"));
  println!();
  println!("{}", h_head("Discover what to run (before `run`)"));
  help_row("scsh list", "Every skill by profile — result path, harness, env (human-readable).");
  help_row("scsh list --json", "Same, as JSON — preferred for scripts and agents.");
  help_row("scsh check-profile <name>", "Exit 0 iff that profile exists with at least one skill (no runtime).");
  help_row("scsh probe [profile…]", "Exit 0 iff at least one of its harness·model routes is runnable here.");
  println!();
  println!("{}", h_head("External config (no install into the target repo)"));
  help_row("--override-dot-scsh-yml <path>", "Use this `.scsh.yml` and its sibling `.skills/` instead of the repo's.");
  println!("{}", h_dim("  The target tree stays clean: skill bodies are injected into the run clone only."));
  println!("{}", h_dim("  Works with `run`, `list`, and `check-profile`. Mutually exclusive with `--def`."));
  println!("{}", h_dim("  Without the flag, a profile the repo's .scsh.yml does not declare falls back to the"));
  println!("{}", h_dim("  global manifest $SCSH_HOME/.scsh.yml (installed by `scsh installskills --global`);"));
  println!("{}", h_dim("  the repo's own config always wins for the profiles it declares."));
  println!();
  println!("{}", h_head("Base commit (what the clone's mainline points at)"));
  help_row("--base <ref>", "Point the run clone's `main` (or `master`) at <ref> for this run only.");
  println!("{}", h_dim("  A skill that reads the committed range `origin/main..HEAD` — every reviewer does"));
  println!("{}", h_dim("  — otherwise sees whatever your local `main` happens to point at. This pins it."));
  println!("{}", h_dim("  Your own repository is never touched — only the throwaway clone is repointed."));
  println!("{}", h_dim("  <ref> is any git revision this repo ALREADY has: a commit sha (full or short),"));
  println!("{}", h_dim("  a branch, a tag (annotated tags are peeled), or a form like HEAD~3. Nothing is"));
  println!("{}", h_dim("  fetched, so `origin/main` is only as fresh as your last fetch."));
  println!("{}", h_dim("  Refused up front when <ref> is not a commit here, when the repo has neither a"));
  println!("{}", h_dim("  `main` nor a `master` branch, or when you are ON that branch (an empty range)."));
  println!();
  println!("{}", h_head("Profile selection"));
  println!("{}", h_dim("  Skills with no `profile:` belong to the reserved `default` profile."));
  println!("{}", h_dim("  A skill with `profile: X` runs only when you select profile X."));
  help_row("scsh run", "Run `default` only (skills with no profile).");
  help_row("scsh run code-review", "Run one named profile.");
  help_row("scsh run a b", "Run several profiles (same as `scsh run --profile a,b`).");
  help_row("scsh run --profile a,b", "Comma/semicolon-separated profile list.");
  println!("{}", h_dim("  If every skill is profiled, bare `scsh run` is a no-op that lists profiles."));
  println!();
  println!("{}", h_head("Preflight (fails fast — message names one fix)"));
  print!(
    r#"    1. git is installed
    2. current directory is inside a git repository
    3. working tree is clean          (scsh clones COMMITTED state only)
    4. .scsh.yml exists and matches the schema
    5. tmp/ is gitignored             (build scratch + results stay untracked)
    6. a container runtime is available (macOS: container → docker → podman;
       otherwise docker → podman; override with SCSH_RUNTIME=<name>)
    7. the runtime engine is running  (scsh prints how to start it)
"#
  );
  println!();
  println!("{}", h_head("Watch it live"));
  println!("{}", h_dim("  Every run registers with the session browser and prints a clickable deep link"));
  println!("{}", h_dim("  (http://127.0.0.1:7274/job/<id>, port from SCSH_DAEMON_PORT) at the start"));
  println!("{}", h_dim("  AND as one of the last lines — recordings, logs, and live TUIs live there."));
  println!();
  println!("{}", h_head("What `run` does (summary)"));
  println!("{}", h_dim("  Builds one image per harness needed (`scsh-opencode`, `scsh-claude`), then runs"));
  println!("{}", h_dim("  every selected skill in parallel — each in its own container. On Linux/docker/podman"));
  println!("{}", h_dim("  the run dir is bind-mounted at /home/agent/repo; on macOS Apple Container scsh"));
  println!("{}", h_dim("  git-pushes into a bare repo and the container clones via a local git daemon."));
  println!("{}", h_dim("  Skills must not git fetch/pull remotes inside. After exit, scsh copies each"));
  println!("{}", h_dim("  Skills with `commits: true` may also bring commits back via local cherry-pick."));
  println!(
    "{}",
    h_dim(
      "  Unavailable harnesses and opencode models are skipped; \
the run fails only when every selected skill is skipped.",
    )
  );
  println!();
  println!("{}", h_head("Exit codes"));
  help_row("0", "Every selected skill that ran finished successfully (skipped harnesses are OK).");
  help_row("non-zero", "At least one skill failed, or every selected skill was skipped/unavailable.");
  println!();
  println!("{}", h_head("Useful environment variables"));
  help_row("SCSH_RUNTIME", "Force container runtime: docker, podman, or container (Apple).");
  help_row(
    "SCSH_GIT_TRANSPORT",
    "Force git push/fetch transport (1) or bind-mount clone (0). Ignored on macOS Apple Container.",
  );
  help_row(
    runtime::GIT_TRANSPORT_HOST_ENV,
    "Override git-daemon host IP inside the container (default: ip route gateway).",
  );
  help_row("SCSH_KEEP_RUNS=1", "Keep every /tmp/scsh-*-run-* clone (also skips stale sweep).");
  help_row("SCSH_NO_OPENCODE_AUTH=1", "Do not forward opencode credentials into containers.");
  help_row("SCSH_NO_CLAUDE_AUTH=1", "Do not forward Claude credentials into containers.");
  help_row("SCSH_NO_CURSOR_AUTH=1", "Do not forward Cursor credentials into containers.");
  println!();
  println!("{}", h_head("After a run"));
  println!("{}", h_dim("  Read each skill's declared `result` path (usually under tmp/). On failure, scsh"));
  println!("{}", h_dim("  prints the kept run-clone path — inspect tmp/scsh-run.log there for full harness output."));
  println!();
  println!("{}", h_head("See also"));
  help_row("scsh help internals", "Repo sync, auth forwarding, live board, image contents.");
  help_row("scsh help .scsh.yml", "Config schema: harness, invocations, env, commits, timeout.");
  help_row("scsh help cache", "When an identical re-run is served from tmp/.sccache/.");
  println!();
}

/// `scsh help .scsh.yml` — the project config file, in full. The YAML sample contains literal
/// `${...}` and `{skill}` placeholders; inlining it into the format string would require
/// doubling every brace and hurt readability, so the lint is allowed here.
#[allow(clippy::print_literal)]
fn print_help_config() {
  println!();
  println!("{} {}", h_head(".scsh.yml"), console::style("\u{2014} the project config file").bold());
  println!("{}", h_dim("The whole file is just your skills; scsh owns the container command. The base"));
  println!(
    "{}",
    h_dim("image is built in (Debian + shared base + per-harness CLI) — no version/project/image header.")
  );
  println!();
  print!(
    "{}",
    r#"  terminal:               # optional: PTY size for harness runs (and their .cast recordings)
    cols: 200             #   default 200 (20..500)
    rows: 50              #   default 50 (10..200)
  skills:                 # one entry per .skills/<name>/ folder

    add:                    #   key must match the skill directory name
      harness: opencode     #     direct run — OR use invocations: for a matrix (below)
                            #     harnesses: opencode | claude | codex | grok | cursor
      model: openai/...     #     optional; the model the harness passes to the tool
      effort: high        #     optional; reasoning effort (codex: minimal..xhigh, grok:
                          #       low..max, cursor: low..high as --model slug suffixes). With
                          #       invocations: a default routes may override; harnesses
                          #       without an effort knob ignore it
      timeout: 600        #     optional; seconds — kill the container & fail if exceeded
      inactivity_timeout: 1800 # optional; seconds the recorded screen may show nothing new
                          #       before the run is killed as stuck. Default 1800 (30 minutes).
                          #       Per-route override under invocations: is allowed too, and a
                          #       workflow def step takes the same key.
      env:                #     optional; host vars to forward (-e) into the container
        - A: ${A}         #       require A — refuse the skill if A is unset
        - B: ${B:-5}      #       forward B, or inject the default 5 when unset
        - X: ${X:?msg}    #       require X, refusing with your message
      profile: extra      #     optional; run only under --profile extra (not by default)
      commits: true       #     optional; bring commits the skill makes back onto your
                          #       branch (rebased; or saved to scsh/incoming/<skill>-…
                          #       if they don't apply cleanly). A real, repeatable side
                          #       effect — run twice and you get the commit twice.
      autoinstall: false  #     optional; default true. false = authoring-only: `installskills`
                          #       won't copy it into a consumer repo (an `internal-` name does the same)
      invocations:        #     optional matrix — each route expands to `{skill}-{route}`
        opencode-gpt:       #       at run and install time; per-route profile/commits/
                            #       inactivity_timeout override
          harness: opencode
          model: openai/...
      result: tmp/x.json  #     required; use {name} in the path when `invocations:` is set
"#
  );
  println!();
  println!("{}", h_head("Env value syntax"));
  println!("{}", h_dim("  scsh resolves each value on the host, then forwards it (or refuses the skill):"));
  help_row("${VAR} or $VAR", "require VAR; refuse the skill if it is unset.");
  help_row("${VAR:-default}", "forward VAR, or inject `default` when unset (${VAR:-} = empty).");
  help_row("${VAR:?message}", "require VAR; refuse with your `message` if unset.");
  help_row("literal", "a bare value like `A: A` is the literal string \"A\".");
  println!();
  println!();
  println!("{}", h_head("Profiles"));
  println!("{}", h_dim("  No `profile:` = the reserved `default` profile (runs on a bare `scsh run`). A skill"));
  println!("{}", h_dim("  with `profile: X` runs only under `--profile X`; pass a list (`--profile a,b`) to run"));
  println!("{}", h_dim("  several. If every skill is profiled, `scsh run` is a no-op that lists the profiles."));
  println!("{}", h_dim("  Discover them programmatically (runtime-free): `scsh list --json`, or gate a script on"));
  println!("{}", h_dim("  `scsh check-profile <name>` (exit 0 iff that profile exists with at least one skill)."));
  println!();
  println!("{}", h_head("Sharing skills (install sources)"));
  println!("{}", h_dim("  When another repo runs `scsh installskills <this-repo>`, scsh installs every skill in"));
  println!("{}", h_dim("  this manifest EXCEPT those marked `autoinstall: false` or named `internal-*` (both"));
  println!("{}", h_dim("  authoring-only), merging the rest verbatim into that repo's own .scsh.yml."));
  println!("{}", h_dim("  Existing skill keys in the consumer are left untouched — scsh warns on conflicts."));
  println!();
  println!("{}", h_dim("Harness commands (inside the container, all recorded as interactive TUIs):"));
  println!("{}", h_dim("  opencode: opencode -m <model> --prompt \"Run the skill defined in .skills/<source>/…\""));
  println!(
    "{}",
    h_dim(
      "  claude:   claude --permission-mode bypassPermissions \"Run the skill defined in .skills/<source>/…\" \
(CLAUDE_CODE_OAUTH_TOKEN or ~/.claude/.credentials.json)",
    )
  );
  println!(
    "{}",
    h_dim(
      "  codex:    codex --dangerously-bypass-approvals-and-sandbox -m <model> \"Run the skill defined in .skills/<source>/…\" \
(~/.codex/auth.json or OPENAI_API_KEY)",
    )
  );
  println!(
    "{}",
    h_dim(
      "  grok:     grok --always-approve -m <model> --effort <level> \"Run the skill defined in .skills/<source>/…\" \
(~/.grok/auth.json or XAI_API_KEY)",
    )
  );
  println!(
    "{}",
    h_dim(
      "  cursor:   cursor-agent --force --sandbox disabled --model <model> \"Run the skill defined in .skills/<source>/…\" \
(macOS keychain / auth.json / CURSOR_API_KEY)",
    )
  );
  println!();
}

/// `scsh help internals` — how a run actually works, end to end.
fn print_help_internals() {
  println!();
  println!("{} {}", h_head("Internals"), console::style("\u{2014} how a run works").bold());
  println!();
  println!("{}", h_head("Preflight order"));
  println!("{}", h_dim("  A real `run` is repo-hygiene-first and fails in this order; the message names"));
  println!("{}", h_dim("  exactly what's wrong and the one command to fix it."));
  print!(
    r#"    1. git is installed
    2. the current directory is inside a git repository
    3. the working tree is clean       (run only; scsh clones COMMITTED state)
    4. .scsh.yml exists, and matches the schema
    5. /tmp is gitignored               (run only; build scratch + results stay untracked)
    6. a container runtime is available (macOS: container -> docker -> podman;
       otherwise docker -> podman; override with SCSH_RUNTIME=podman)
    7. the runtime's engine is running  (run only; scsh prints how to start it)
    (list runs only the non-run checks: git, repo, config, runtime.)
"#
  );
  println!();
  println!("{}", h_head("How a run works"));
  print!(
    r#"  scsh builds the shared base image (`scsh-base:latest`) first — or skips it when the tag
  already matches the embedded Dockerfile fingerprint — then builds the needed per-harness
  images (`scsh-opencode`, `scsh-claude`, `scsh-codex`, `scsh-grok`, `scsh-cursor`) on top IN PARALLEL,
  skipping any whose tag already matches. Each build is version-checked during the build. Then, for
  EVERY selected skill in parallel, it prepares a /tmp run dir (scsh-YYYYMMDD-HHMMSS-utc-run-<invocation> on docker/podman,
  or scsh-<nonce>-run-<invocation> on Apple container — ≤ 64 chars, middle-truncated with .. when
  needed) and runs the skill's harness in its own container. On docker/podman/Linux the host
  git-clones into the run dir and bind-mounts it at /home/agent/repo. On macOS Apple Container
  scsh git-pushes into a bare transport repo and the container clones from a per-run git daemon
  (only run_dir/tmp is bind-mounted — results, logs, forwarded auth). The repo lives UNDER the
  agent's home, not as it, so harness scratch stays out of the tree.

  scsh injects SCSH_RESULT=<result path> into every container so one skill folder can
  serve multiple invocations with different result files.

  Repo sync — push IN, pull OUT (never GitHub from inside the container):
  Host push IN: git clone + bind-mount (docker/podman/Linux), or git push to transport.git +
  container git clone via git:// (Apple Container on macOS). Skills must not git fetch, pull,
  push, or clone remotes inside. After the container exits, scsh on the HOST pulls OUT: (1) the
  result file — always (from bind-mounted tmp/); (2) new commits — only when commits: true AND
  the skill committed — via local git fetch from the run clone or pull.git and cherry-pick.
  scsh never pushes to any remote. Reviewer skills are review-only (no commits).

  Each skill MUST produce its declared `result` file. Missing after the container
  exits -> that skill fails and the whole invocation exits non-zero; otherwise scsh
  copies the result back into your repo, moving any existing file aside to
  <name>.bak.YYYYMMDD-HHMMSS-utc. All skills run regardless, so one run reports
  every skill's outcome.

  Auth: opencode skills copy the host ~/.local/share/opencode/auth.json and
  ~/.config/opencode/opencode.json (plus optional opencode.jsonc) into each run clone,
  then bind-mount from there (needed for custom providers such as Nebius GLM;
  opt out: SCSH_NO_OPENCODE_AUTH=1).
  Claude skills use host CLAUDE_CODE_OAUTH_TOKEN (from `claude setup-token`) and/or
  ~/.claude/.credentials.json, copied into the run dir and bind-mounted into the container
  (opt out: SCSH_NO_CLAUDE_AUTH=1).
  Codex skills copy the host ~/.codex/auth.json and config.toml (from `codex login`) into the
  run clone's gitignored tmp/.codex — the image's CODEX_HOME — and forward OPENAI_API_KEY when
  set; the credentials are scrubbed from the run dir after the container exits
  (opt out: SCSH_NO_CODEX_AUTH=1). Codex is the recommended native harness for GPT models.
  Grok skills work the same way: host ~/.grok/auth.json + config.toml (from `grok login`)
  are copied into tmp/.grok — the image's GROK_HOME — XAI_API_KEY is forwarded when set,
  and credentials are scrubbed after exit (opt out: SCSH_NO_GROK_AUTH=1). Grok is the
  recommended native harness for Grok models.
  Cursor skills copy the host ~/.cursor/cli-config.json and optional mcp.json into tmp/.cursor,
  OAuth tokens from ~/.config/cursor/auth.json or the macOS login keychain into tmp/.config/cursor/auth.json,
  CURSOR_API_KEY is forwarded when set, and credentials are scrubbed after exit (opt out: SCSH_NO_CURSOR_AUTH=1). Cursor is
  the native harness for Cursor Agent models (Composer, etc.).
  Harness runs at full verbosity (OpenCode DEBUG + --print-logs; Claude --verbose --debug;
  Codex RUST_LOG tracing + its final message appended to the log; Grok --debug + its debug
  log appended; Cursor --output-format stream-json);
  every line is teed to tmp/scsh-run.log and the session browser daemon (opt out: SCSH_QUIET=1).
  A transient infra failure (timeout, provider overload, container/clone error) is retried once on a fresh
  clone (opt out: SCSH_NO_RETRY=1); failures land in `scsh failures` with stable reason codes.
  Every skill outcome is also recorded durably in ~/.scsh/stats.jsonl — route, duration,
  attempts, and the branch workload (commits + LOC over main) — browse with `scsh stats`.
  Unavailable harnesses and opencode models are skipped; a run fails only when every selected skill is skipped.
  Every line of harness output is teed to <run_dir>/tmp/scsh-run.log for inspection.

  The Dockerfile is embedded at compile time (`src/Dockerfile`). docker/podman get it
  on stdin; Apple Containers gets a comment-stripped copy written to a temp context dir
  (Apple has no stdin build, and rejects Dockerfiles ≥ 16KB — apple/container#735).
  Your repository is modified only by the result copies (into the gitignored tmp/).

  Cleanup: a skill's container is --rm, and its /tmp clone is host-side scratch. After a
  SUCCESSFUL skill scsh removes that clone; a FAILED skill's clone is kept for inspection
  (its path is printed). Stale clones from past runs (>24h old) are swept at the next run's
  start. Keep every clone with SCSH_KEEP_RUNS=1 (also skips the sweep).

  The live board: on a terminal the build and every skill are drawn as collapsible rows,
  inline in the normal buffer (no alternate screen, so your scrollback keeps working). Each row
  carries a [0]..[9], [A]..[Z] label on the left: press that digit or letter to expand/collapse the row
  (scsh turns on the terminal's keyboard-enhancement protocol so Ctrl+digit works too; without it, the
  plain digit toggles — or click the row if the mouse is forwarded). Expanding shows the proc's
  output, each line stamped with its time relative to that proc's start. SCROLL with the wheel,
  ↑↓, PgUp/PgDn or Home/End (e/c expand/collapse all; Ctrl-C aborts). On finish scsh wipes the
  live region and leaves a compact ✓/✗ summary. Off a TTY it falls back to plain ▶ / ✓ / ✗ lines.
"#
  );
  println!();
  println!("{}", h_head("What's in the image"));
  print!(
    r#"  A glibc Debian-slim base, baked with a broad dev/CLI toolchain so skills work with
  no setup step. Built once, then cached and reused (the first run does the build):
    languages/build  python3 (+ uv), Go, Rust (cargo), C/C++ (gcc/g++/make/cmake),
                     perl, gawk, node
    harness images   scsh-opencode (+ opencode-ai), scsh-claude (+ @anthropic-ai/claude-code)
    data/CLI         jq, yq, ripgrep, shellcheck, git (+ git-lfs), gh, sqlite3,
                     psql, protoc, curl/wget, tar/gzip/xz/zip/unzip, patch, tree
    cloud            aws (v2), gcloud + gsutil, kubectl
    networking       ping, traceroute, dig/nslookup, nc, ss/ip, whois, socat
  Java is intentionally NOT installed (nothing here is JVM; a JDK adds ~300 MB).
  The image is built with the TIMEZONE OF THE MACHINE BUILDING IT (scsh passes the
  host's TZ as a build arg), so timestamps a skill produces match your machine.
  It is platform-agnostic: the same Dockerfile builds on x86_64 and arm64 (arch is
  resolved at build time, no hardcoded-arch downloads). On macOS, Apple Containers is
  the default runtime — keep src/Dockerfile under 15KB so builds stay under Apple's
  16KB gRPC header limit.
"#
  );
  println!();
}

/// `scsh help def` — the harness-definition format: flat one-shot tasks, workflow DAGs, and
/// the two loop forms (fixed `repeat`, agent-driven `do-while` with loop-carried inputs).
fn print_help_defs() {
  println!();
  println!(
    "{} {}",
    h_head("Harness definitions"),
    console::style("\u{2014} run one task or a workflow, no .scsh.yml needed").bold()
  );
  println!();
  println!("{}", h_dim("  A definition is a YAML file describing a runnable job: `scsh run --def <name>`."));
  println!(
    "{}",
    h_dim("  Discovery (later shadows earlier): built-ins  <  ~/.harness/<name>.yml  <  <repo>/.harness/<name>.yml.")
  );
  println!("{}", h_dim("  Params are declared under `params:` and supplied as environment variables at run time."));
  println!();
  println!("{}", h_head("Flat form — one task, many agents"));
  print!(
    r#"  description: "…"        one line, shown in listings
  params:                  typed inputs (string/int/bool/enum), optional `default:`
    A: {{ type: int, default: 2 }}
  task: |                  the prompt; materialized into the run clone as .skills/<name>/SKILL.md
  invocations:             the agent matrix — same schema as a .scsh.yml skill's invocations
    c: {{ harness: claude, model: sonnet }}
"#
  );
  println!();
  println!("{}", h_head("Workflow form — a DAG of steps"));
  print!(
    r#"  steps:                   a block map; each step runs on its own agent, in its own clone
    <id>:
      agent:               harness: claude|codex|cursor|grok|opencode; optional model:, effort:
      prompt: |            intent only — scsh appends the machine I/O contract
      inputs:              env vars for the step:  NAME: params.X  or  NAME: stepid.field
      output:              typed result fields the step must write to $SCSH_RESULT (JSON)
        n: {{ type: int }}   types: string | int | bool | enum (with `choices: a, b, c`) | string_list | object
      needs: a, b          DAG edges — steps whose completion this step waits for
      when:                gate — run only if every condition holds (else the step is skipped)
        a.kind: code       scalar = equality; or one operator: eq/ne/lt/lte/gt/gte/in
      artifacts: out.txt   extra files written next to $SCSH_RESULT, copied to the session dir
      commits: true        bring the step's commits back onto the caller's branch (packdiff'd)
      commit-identity:     who authors those commits: `notes` (default — the recognizable scsh
                           bot, excluded from review as the notes author) or `runner` (the
                           person running the pipeline, from this repo's git user.name/email)
"#
  );
  println!();
  println!("{}", h_head("Loops"));
  print!(
    r#"  repeat: 3              run this one step N times, sequentially — each iteration is its
                         own run and commit boundary; the job graph grows as iterations start.
  do-while: <first-step>   on the loop body's FINAL step, naming the body's FIRST step. The body
                         (everything between them, per `needs`) runs at least once, then repeats
                         while the final step's result JSON sets the boolean
                         `SCSH_DO_WHILE_REPEAT` to true. scsh has no comparison language here —
                         an agent decides. A backstop caps runaway loops at 25 iterations.
  max-iterations: 5      on the same do-while step: THIS loop's own ceiling, below the backstop.
                         Reaching it fails the job with a message naming the cap, so an
                         unattended loop cannot burn a fleet per round until the backstop. Use
                         it whenever a loop's real budget is small; `repeat` needs no cap
                         because it already declares its exact count.
  break: true            on the loop body's FIRST step, lets that step exit before the rest of
                         the body runs. It must declare the boolean output `SCSH_LOOP_BREAK`;
                         true exits this loop, false continues through the body normally.
  Loop-carried inputs:   a body step may reference the FINAL step's output with no `needs:` edge
                         — it receives the PREVIOUS iteration's value (empty on round one). This
                         is the loop's data channel: feedback flows between rounds as typed
                         outputs under gitignored tmp/, never as committed files.
  Loop freshness:        body steps receive `SCSH_LOOP_ITERATION` (1-based). From iteration 2 on,
                         inputs bound to steps OUTSIDE the body bind to the empty string — data
                         from before the loop is round-0 history, not current state. The SCSH_
                         input-name prefix is reserved for scsh.
"#
  );
  println!();
  println!("{}", h_head("Retries and resume"));
  print!(
    r#"  A failed task retries automatically — fresh clone, fresh container, a new attempt row
  linked to the failed one — up to 5 times and for at most its wall-clock retry budget
  (default 30m). Retryable:
  container/runtime trouble, provider overload/disconnects, and non-zero harness exits (a
  harness dying at startup is infrastructure). Retries back off exponentially with jitter;
  a route failing the SAME way 5 times in a row trips a circuit breaker instead of burning
  tokens until dawn. An invalid result gets one schema-correction retry. Tune per skill,
  route, or def step with retry_for: (90s/45m/8h) and retry_signature_cap:, or run-wide
  with SCSH_RETRY_FOR / SCSH_RETRY_SIGNATURE_CAP; SCSH_NO_RETRY=1 disables everything.
  Beyond one run, every job is presumed worth finishing: on terminal failure the daemon
  restarts it (resuming completed workflow steps) with 5m→60m backoff, up to the job's
  restart budget — 25 by default, or `scsh run --retries N`, 0 to opt out — stopping
  early after 3 identical job
  failures or a human's Force stop.
  scsh run --def <name> --resume-from <session>   restores every step whose validated result
  the named session already produced (loop iterations included, commits already on the branch)
  and runs only the rest. The session browser's "Restart remaining" button on a failed job
  does exactly this; "Restart from scratch" re-runs everything.
"#
  );
  println!();
  println!(
    "{}",
    h_head("Executable examples (built in — read them with `scsh run --def <name>` or in src/harness_defs/)")
  );
  help_row("demo-loop-repeat", "Fixed loop: increment and commit number.txt three times.");
  help_row("demo-loop-do-while", "Agent-driven loop: increment until a compare step says stop.");
  help_row("demo-loop-break", "Early exit: check first, then skip the rest of the loop body.");
  help_row("gorgeous-pipeline", "The review loop on YOUR branch: prepare PR-DESCRIPTION.md, then the 15-route");
  help_cont("Opus/Codex/Cursor fleet loops decide -> fix -> review until the approval bar is met.");
  help_cont("Feedback rides the loop-carried channel, so review comments never enter git history.");
  println!();
}

/// `scsh help cache` — the content-addressed result cache.
fn print_help_cache() {
  println!();
  println!("{} {}", h_head("Cache"), console::style("\u{2014} content-addressed skill results").bold());
  println!();
  println!("{}", h_dim("scsh caches each skill's result and reuses it when nothing that matters changed —"));
  println!("{}", h_dim("a cache hit returns the result instantly, with no clone, no container, no model call."));
  println!();
  println!("{}", h_head("The cache key"));
  print!(
    r#"  Before running a skill, scsh hashes (sha256) a deterministic blob of:
    • the repo's committed content (the git HEAD tree),
    • the skill's own files (SKILL.md + scripts), and
    • the resolved environment forwarded to the skill (sorted).
  Same commit + same skill + same env  =>  same key  =>  a hit. Change any of them
  (edit a file, pass A=9, tweak the skill) and the key changes => a miss.
"#
  );
  println!();
  println!("{}", h_head("Where it lives"));
  print!(
    r#"  Under the repo's gitignored tmp/: tmp/.sccache/<sha256>.json (plus .cast / .chapters.json),
  and nowhere else. Each entry holds the skill's result, when it was cached, the original
  duration and recording, chapters once annotation finishes, AND — for a commit-enabled skill —
  the commits it made (journaled as a git patch). On a hit scsh restores the result file, prints
  it with "(cached · <when>)", restores the cast + chapters, and replays any journaled commits;
  on a miss it runs and stores them.
"#
  );
  println!();
  println!("{}", h_head("Commits are journaled and replayed (a hit reproduces them)"));
  print!(
    r#"  A commit-enabled skill (commits: true) changes the repo when it commits, so the very
  next run sees a NEW HEAD tree => a different key => a miss => it runs (and commits) again.
  But the commits ARE journaled in the cache. Revert to the same committed state (e.g.
  git reset --hard to before the skill's commit) and run again => the key matches => a HIT:
  scsh restores the result AND replays the journaled commits, so the commit reappears on top.
  A hit reproduces the full side effect, not just the result. (If a replay can't apply
  cleanly, scsh saves the patch under tmp/.sccache/ and leaves your branch alone.)
"#
  );
  println!();
  println!("{}", h_head("The author you'll recognize (a tripwire)"));
  print!(
    r#"  scsh stamps the commits a skill makes with a deliberately unmistakable author —
  dkorolev-neon-elon-bot <dmitry.korolev+elon-presley@gmail.com> (yes, a neon-cyberpunk
  Elon). It is never a real contributor. These commits are LOCAL-ONLY by design: scsh
  rebases them onto your branch, it never pushes. So if that face ever shows up in a code
  review or a pushed commit list, that's your signal — you pushed something you shouldn't
  have. Go check.
"#
  );
  println!();
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn apple_shell_response_recovery_requires_valid_json_and_inner_exit_zero() {
    assert!(apple_container_lost_shell_response(Some(
      r#"failed to send signal: ["signal": 15, "error": invalidArgument: "missing signal in xpc message"]"#
    )));
    assert!(!apple_container_lost_shell_response(Some("ordinary harness failure")));

    let dir = std::env::temp_dir().join(format!("scsh-graceful-{}", runtime::random_nonce_6()));
    std::fs::create_dir_all(dir.join("tmp/scsh/job")).unwrap();
    std::fs::write(dir.join(format!("{}.exit", runtime::RUN_LOG_REL)), "0\n").unwrap();
    std::fs::write(dir.join("tmp/scsh/job/result.json"), r#"{"result":"ok"}"#).unwrap();
    assert!(inner_harness_result_is_good(&dir, "tmp/scsh/job/result.json", false));
    std::fs::write(dir.join(format!("{}.exit", runtime::RUN_LOG_REL)), "1\n").unwrap();
    assert!(!inner_harness_result_is_good(&dir, "tmp/scsh/job/result.json", false));
    let _ = std::fs::remove_dir_all(dir);
  }

  #[test]
  fn interrupted_harness_with_a_result_rejoins_the_normal_validation_path() {
    let dir = std::env::temp_dir().join(format!("scsh-inactive-result-{}", runtime::random_nonce_6()));
    let result = "tmp/scsh/job/result.json";
    std::fs::create_dir_all(dir.join("tmp/scsh/job")).unwrap();

    assert!(!interrupted_harness_result_is_recoverable(&dir, result));
    // Even a malformed file crosses the durable-output boundary here: this helper must not
    // duplicate or weaken the workflow schema validator that runs after collection.
    std::fs::write(dir.join(result), "not valid JSON yet").unwrap();
    assert!(interrupted_harness_result_is_recoverable(&dir, result));
    let _ = std::fs::remove_dir_all(dir);
  }

  #[test]
  fn workflow_results_are_strictly_typed_before_they_become_terminal() {
    let outputs = [
      harness_def::OutputField {
        name: "grade".into(),
        ty: harness_def::OutputType::Enum,
        choices: vec!["excellent".into(), "good".into()],
      },
      harness_def::OutputField { name: "comments".into(), ty: harness_def::OutputType::StringList, choices: vec![] },
    ];
    let contract = WorkflowResultContract { outputs: &outputs, require_do_while_repeat: true };
    let valid =
      extract_step_outputs(r#"{"grade":"good","comments":["one","two"],"SCSH_DO_WHILE_REPEAT":false}"#, contract)
        .unwrap();
    assert_eq!(valid.get("grade").map(String::as_str), Some("good"));
    assert_eq!(valid.get("comments").map(String::as_str), Some(r#"["one","two"]"#));
    assert_eq!(valid.get("SCSH_DO_WHILE_REPEAT").map(String::as_str), Some("false"));

    let missing = extract_step_outputs(r#"{"grade":"good","comments":[]}"#, contract).unwrap_err();
    assert!(missing.contains("SCSH_DO_WHILE_REPEAT") && missing.contains("missing"));
    let wrong =
      extract_step_outputs(r#"{"grade":"good","comments":"one\n\ntwo","SCSH_DO_WHILE_REPEAT":false}"#, contract)
        .unwrap_err();
    assert!(wrong.contains("array of strings"));
    let extra = extract_step_outputs(
      r#"{"grade":"good","comments":[],"comment_count":0,"SCSH_DO_WHILE_REPEAT":false}"#,
      contract,
    )
    .unwrap_err();
    assert!(extra.contains("undeclared field") && extra.contains("comment_count"));
  }

  #[test]
  fn object_outputs_accept_json_objects_and_reject_everything_else() {
    let outputs =
      [harness_def::OutputField { name: "routes".into(), ty: harness_def::OutputType::Object, choices: vec![] }];
    let contract = WorkflowResultContract { outputs: &outputs, require_do_while_repeat: false };
    let valid =
      extract_step_outputs(r#"{"routes":{"conventions-opus":{"grade":"good","comments":[]}}}"#, contract).unwrap();
    assert_eq!(
      valid.get("routes").map(String::as_str),
      Some(r#"{"conventions-opus":{"grade":"good","comments":[]}}"#),
      "objects forward downstream as compact JSON"
    );
    let wrong = extract_step_outputs(r#"{"routes":"a prose blob"}"#, contract).unwrap_err();
    assert!(wrong.contains("must be a JSON object"), "{wrong}");
  }

  #[test]
  fn workflow_step_headline_glimpses_declared_scalar_outputs() {
    // A reviewer step's {grade, comments} has no `result`/`message` field, so without the
    // glimpse its headline would fall back to the tmp/ result path.
    let reviewer = [
      harness_def::OutputField {
        name: "grade".into(),
        ty: harness_def::OutputType::Enum,
        choices: vec!["excellent".into(), "good".into()],
      },
      harness_def::OutputField { name: "comments".into(), ty: harness_def::OutputType::StringList, choices: vec![] },
    ];
    let contract = WorkflowResultContract { outputs: &reviewer, require_do_while_repeat: false };
    let outputs = extract_step_outputs(r#"{"grade":"good","comments":["one"]}"#, contract).unwrap();
    assert_eq!(workflow_outputs_glimpse(contract, &outputs).as_deref(), Some("grade: good"));

    // Scalars keep contract order; string values keep only their first line; list/object
    // fields never appear.
    let collect = [
      harness_def::OutputField { name: "approved".into(), ty: harness_def::OutputType::Bool, choices: vec![] },
      harness_def::OutputField { name: "verdict".into(), ty: harness_def::OutputType::String, choices: vec![] },
      harness_def::OutputField { name: "feedback".into(), ty: harness_def::OutputType::Object, choices: vec![] },
    ];
    let contract = WorkflowResultContract { outputs: &collect, require_do_while_repeat: false };
    let outputs = extract_step_outputs(
      r#"{"approved":false,"verdict":"not met\ndetails follow","feedback":{"mean":4.2}}"#,
      contract,
    )
    .unwrap();
    assert_eq!(workflow_outputs_glimpse(contract, &outputs).as_deref(), Some("approved: false · verdict: not met"));

    // A long value clips to 64 chars with an ellipsis; an empty string is skipped, and
    // with nothing scalar left there is no glimpse at all.
    let decide = [harness_def::OutputField {
      name: "change_request".into(),
      ty: harness_def::OutputType::String,
      choices: vec![],
    }];
    let contract = WorkflowResultContract { outputs: &decide, require_do_while_repeat: false };
    let long = "x".repeat(80);
    let outputs = extract_step_outputs(&format!(r#"{{"change_request":"{long}"}}"#), contract).unwrap();
    let expected = format!("change_request: {}…", "x".repeat(63));
    assert_eq!(workflow_outputs_glimpse(contract, &outputs).as_deref(), Some(expected.as_str()));
    let outputs = extract_step_outputs(r#"{"change_request":""}"#, contract).unwrap();
    assert_eq!(workflow_outputs_glimpse(contract, &outputs), None);
  }

  #[test]
  fn workflow_schema_repair_keeps_the_invocation_identity_and_names_the_error() {
    let definition = harness_def::builtin_defs()
      .into_iter()
      .find_map(|(name, source)| {
        (name == "demo-loop-do-while").then(|| harness_def::validate(name, source, harness_def::DefSource::Builtin))
      })
      .expect("built-in definition exists")
      .expect("built-in definition validates");
    let step = definition.steps.first().expect("demo has a first step");
    let invocation = step_invocation(step, "increment", "tmp/scsh/session", Vec::new(), None);
    let repaired = schema_repair_invocation(&invocation, "result is missing the 'value' field");
    assert_eq!(repaired.name, invocation.name);
    assert_eq!(repaired.result, invocation.result);
    match repaired.delivery {
      config::SkillDelivery::DirectPrompt(prompt) => {
        assert!(prompt.contains("Result correction retry"));
        assert!(prompt.contains("missing the 'value' field"));
        assert!(prompt.contains("only correction retry"));
      }
      _ => panic!("workflow steps always use a direct prompt"),
    }
  }

  #[test]
  fn retry_decision_is_budgeted_breaker_capped_and_browser_restarts_always_win() {
    use RetryDecision::{Automatic, Browser, Schema, Stop, StopBreaker};
    let policy = failure::RetryPolicy {
      max_retries: 5,
      budget_secs: 30 * 60,
      backoff_initial_secs: 60,
      backoff_cap_secs: 15 * 60,
      signature_cap: 5,
    };
    let decide = |reason: &'static str, retries: u32, spent: u64, identical: u32| {
      retry_decision(Some(reason), false, false, policy, retries, spent, identical, true, true, true)
    };

    // A harness dying with a non-zero exit is retryable — a silent crash at container
    // startup is infrastructure, and both real 2026-07-16 pipeline failures were exactly
    // that. Both ceilings apply: retry number five may run while time remains, then the
    // count stops a sixth retry; a slow sequence can exhaust the wall clock first.
    assert_eq!(decide(failure::reason::HARNESS_NONZERO, 0, 0, 1), Automatic);
    assert_eq!(decide(failure::reason::CONTAINER_TIMEOUT, 4, 29 * 60, 1), Automatic);
    assert_eq!(decide(failure::reason::CONTAINER_TIMEOUT, 5, 0, 1), Stop);
    assert_eq!(decide(failure::reason::CONTAINER_TIMEOUT, 0, 30 * 60, 1), Stop);
    assert_eq!(decide(failure::reason::HARNESS_DISCONNECTED, 0, 0, 1), Automatic);
    // The identical-failure breaker trips ahead of the budget, with its own verdict.
    assert_eq!(decide(failure::reason::HARNESS_NONZERO, 0, 0, 5), Automatic);
    assert_eq!(decide(failure::reason::HARNESS_NONZERO, 0, 0, 6), StopBreaker);
    // Non-transient reasons fail fast in-run; the job supervisor's restarts own them.
    assert_eq!(decide(failure::reason::RESULT_INVALID, 0, 0, 1), Stop);
    // The one schema-correction retry precedes the budget logic.
    assert_eq!(
      retry_decision(Some(failure::reason::RESULT_INVALID), false, true, policy, 0, 0, 1, true, true, true),
      Schema
    );
    // SCSH_NO_RETRY (retry_enabled=false) stops everything except explicit browser clicks.
    assert_eq!(
      retry_decision(Some(failure::reason::CONTAINER_TIMEOUT), false, false, policy, 0, 0, 1, false, true, true),
      Stop
    );
    assert_eq!(
      retry_decision(
        Some(failure::reason::HARNESS_NONZERO),
        true,
        false,
        policy,
        u32::MAX,
        u64::MAX,
        99,
        false,
        true,
        true,
      ),
      Browser,
      "a browser restart ignores budget, breaker, and SCSH_NO_RETRY"
    );
    // First-attempt auth rejection is permanent; on a later attempt it is flakiness.
    assert_eq!(
      retry_decision(Some(failure::reason::HARNESS_AUTH_REJECTED), false, false, policy, 0, 0, 1, true, true, true),
      Stop
    );
    assert_eq!(
      retry_decision(Some(failure::reason::HARNESS_AUTH_REJECTED), false, false, policy, 0, 0, 1, true, true, false),
      Automatic
    );
  }
  use std::ffi::OsString;

  #[test]
  fn daemon_version_staleness_nudges_only_on_a_real_mismatch() {
    // Same build: no nudge.
    assert!(!daemon_version_is_stale("1.29.5", "1.29.5"));
    assert!(!daemon_version_is_stale("1.29.5 (abc1234)", "1.29.5 (abc1234)"));
    // Upgraded binary, daemon still on the old build: nudge.
    assert!(daemon_version_is_stale("1.29.4", "1.29.5"));
    assert!(daemon_version_is_stale("1.29.5 (aaaaaaa)", "1.29.5 (bbbbbbb)"));
    // A daemon too old to serve the version endpoint reports "unknown …" — the status
    // line already explains that; a restart-nudge would be misleading, so suppress it.
    assert!(!daemon_version_is_stale("unknown (older than this feature)", "1.29.5"));
  }

  #[test]
  fn stale_annotation_markers_self_heal_and_fresh_ones_block() {
    let dir = std::env::temp_dir().join(format!("scsh-marker-{}", runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    let cast = dir.join("run.cast");
    std::fs::write(&cast, "{}\n").unwrap();

    // No marker: nothing in progress.
    assert!(!annotation_in_progress(&cast));

    // A fresh marker blocks (another process is honestly annotating) and survives the check.
    let marker = annotation_marker(&cast);
    std::fs::write(&marker, "").unwrap();
    assert!(annotation_in_progress(&cast));
    assert!(marker.exists(), "a fresh marker is honored, not deleted");

    // A stale marker — the annotating process was killed before its cleanup trap ran — is
    // deleted on sight, so one interrupted annotation never blocks the cast forever.
    let stale = std::time::SystemTime::now() - (ANNOTATION_MARKER_TTL + Duration::from_secs(60));
    std::fs::File::options().write(true).open(&marker).unwrap().set_modified(stale).unwrap();
    assert!(!annotation_in_progress(&cast));
    assert!(!marker.exists(), "the stale marker was removed");
    // …and the next check starts clean.
    assert!(!annotation_in_progress(&cast));
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn parent_session_from_cast_path_extracts_id() {
    assert_eq!(
      parent_session_from_cast_path("/Users/x/.scsh/sessions/abcdef/casts/foo.cast").as_deref(),
      Some("abcdef")
    );
    assert_eq!(parent_session_from_cast_path("/tmp/orphan.cast"), None);
    assert_eq!(parent_session_from_cast_path("/sessions/"), None);
  }

  #[test]
  fn atomic_write_lands_whole_and_failure_keeps_the_previous_file() {
    let base = std::env::temp_dir().join(format!("scsh-atomicwrite-{}", runtime::random_nonce_6()));
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join("state.txt");

    atomic_write(&path, b"first").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    atomic_write(&path, b"second").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"second");
    // No `.tmp.<pid>` sibling survives a successful write.
    assert_eq!(std::fs::read_dir(&base).unwrap().count(), 1);

    // Simulate a write that cannot complete: a directory squats on the temp path, so the
    // temp file cannot be created — the previous complete file must remain untouched.
    let tmp = base.join(format!("state.txt.tmp.{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    assert!(atomic_write(&path, b"third").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"second");

    std::fs::remove_dir_all(&base).unwrap();
  }

  #[test]
  fn persist_run_artifacts_copies_cast_and_logs_with_shared_stem() {
    let _env = runtime::test_env_lock();
    let base = std::env::temp_dir().join(format!("scsh-persist-test-{}", runtime::random_nonce_6()));
    let home = base.join("scsh-home");
    let run_dir = base.join("run");
    std::fs::create_dir_all(run_dir.join("tmp")).unwrap();
    std::fs::write(run_dir.join(runtime::RUN_CAST_REL), "cast-bytes").unwrap();
    std::fs::write(run_dir.join(runtime::RUN_LOG_REL), "log-bytes").unwrap();
    std::fs::write(run_dir.join(format!("{}.debug", runtime::RUN_LOG_REL)), "debug-bytes").unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // Pin SCSH_HOME so the durable dirs land under our temp tree (not the developer's ~/.scsh).
    let prev = std::env::var_os("SCSH_HOME");
    std::env::set_var("SCSH_HOME", &home);
    let cast = persist_run_artifacts("sessab", &run_dir, "add", 1_700_000_000).unwrap();
    match prev {
      Some(v) => std::env::set_var("SCSH_HOME", v),
      None => std::env::remove_var("SCSH_HOME"),
    }

    // Everything a session produces lives under ITS OWN id: sessions/<id>/{casts,logs}.
    let casts_dir = home.join("sessions").join("sessab").join("casts");
    assert!(cast.starts_with(casts_dir.to_string_lossy().as_ref()), "cast under sessions/<id>/casts: {cast}");
    let stem = std::path::Path::new(&cast).file_stem().unwrap().to_string_lossy().into_owned();
    assert!(stem.starts_with("add-"), "stem starts with skill name: {stem}");
    assert_eq!(std::fs::read_to_string(&cast).unwrap(), "cast-bytes");
    let logs = home.join("sessions").join("sessab").join("logs");
    assert_eq!(std::fs::read_to_string(logs.join(format!("{stem}.log"))).unwrap(), "log-bytes");
    assert_eq!(std::fs::read_to_string(logs.join(format!("{stem}.debug.log"))).unwrap(), "debug-bytes");
    let _ = std::fs::remove_dir_all(&base);
  }

  #[test]
  fn opencode_auth_path_prefers_xdg_then_home() {
    let xdg = OsString::from("/data");
    let home = OsString::from("/home/u");
    assert_eq!(runtime::opencode_auth_in(Some(&xdg), Some(&home)), Some(PathBuf::from("/data/opencode/auth.json")));
    // No XDG → HOME/.local/share.
    assert_eq!(
      runtime::opencode_auth_in(None, Some(&home)),
      Some(PathBuf::from("/home/u/.local/share/opencode/auth.json"))
    );
    // Empty XDG falls back to HOME too.
    let empty = OsString::from("");
    assert_eq!(
      runtime::opencode_auth_in(Some(&empty), Some(&home)),
      Some(PathBuf::from("/home/u/.local/share/opencode/auth.json"))
    );
    // Nothing to go on → None.
    assert_eq!(runtime::opencode_auth_in(None, None), None);
  }

  #[test]
  fn sweep_removes_only_matching_stale_run_dirs() {
    let base = std::env::temp_dir().join(format!("scsh-sweeptest-{}-{}", std::process::id(), now_secs()));
    std::fs::create_dir_all(&base).unwrap();
    // A matching run-dir, a non-matching scsh dir, an unrelated dir, and a matching *file*.
    let run = base.join("scsh-20231114-221320-utc-run-add");
    let run_apple = base.join("scsh-abcdef-run-add");
    let install = base.join("scsh-installskills-1-2");
    let other = base.join("some-other-dir");
    let run_file = base.join("scsh-19700101-000000-utc-run-x"); // a file, not a dir
    std::fs::create_dir(&run).unwrap();
    std::fs::create_dir(&run_apple).unwrap();
    std::fs::create_dir(&install).unwrap();
    std::fs::create_dir(&other).unwrap();
    std::fs::write(&run_file, b"").unwrap();
    let now = now_secs();

    // A threshold beyond any real age sweeps nothing (an in-progress run is safe).
    assert_eq!(sweep_stale_run_dirs_in(&base, now, u64::MAX), 0);
    assert!(run.is_dir());

    // A zero threshold makes every just-created entry "stale" — but only matching
    // DIRECTORIES are removed; a non run-dir, an unrelated dir, and a matching file are left.
    assert_eq!(sweep_stale_run_dirs_in(&base, now, 0), 2);
    assert!(!run.exists(), "the UTC-stamped run dir is removed");
    assert!(!run_apple.exists(), "the Apple-container run dir is removed");
    assert!(install.is_dir(), "a non run-dir scsh dir is left alone");
    assert!(other.is_dir(), "an unrelated dir is left alone");
    assert!(run_file.is_file(), "a matching *file* (not a dir) is left alone");

    std::fs::remove_dir_all(&base).unwrap();
  }

  #[test]
  fn base_flag_parses_and_is_run_only() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(cli(&["run", "code-review", "--base", "origin/main"]).unwrap().base.as_deref(), Some("origin/main"));
    // Works with a definition run too — that is the gorgeous-pipeline case.
    assert_eq!(cli(&["run", "--def", "greet", "--base", "abc123"]).unwrap().base.as_deref(), Some("abc123"));
    // An ordinary run leaves it unset, so nothing about today's behavior changes.
    assert_eq!(cli(&["run"]).unwrap().base, None);
    // A missing or empty value, and any non-run command, are refused.
    assert!(cli(&["run", "--base"]).is_err());
    assert!(cli(&["run", "--base", "  "]).is_err());
    assert!(cli(&["list", "--base", "origin/main"]).is_err());
  }

  #[test]
  fn resolve_run_base_refuses_bad_refs_and_the_mainline_itself() {
    let tmp = std::env::temp_dir().join(format!("scsh-resolve-base-{}-{}", std::process::id(), now_secs()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let git = |args: &[&str]| {
      git_command().arg("-C").arg(&tmp).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    };
    git_command().args(["init", "-q"]).arg(&tmp).status().unwrap();
    git(&["config", "user.email", "reviewer@example.invalid"]);
    git(&["config", "user.name", "reviewer"]);
    std::fs::write(tmp.join("f"), "base").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-qm", "base"]);
    git(&["branch", "-M", "main"]);
    let base_sha = git_capture(&tmp, &["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // On main itself: reviewing main against main is an empty range, so it is refused.
    let on_main = resolve_run_base(&tmp, &base_sha).unwrap_err();
    assert!(on_main.contains("cannot be used while on 'main'"), "{on_main}");

    git(&["checkout", "-q", "-b", "feature"]);
    std::fs::write(tmp.join("f"), "feature").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-qm", "feature"]);

    // From the feature branch the base resolves, naming the branch the clone will repoint.
    let resolved = resolve_run_base(&tmp, &base_sha).unwrap();
    assert_eq!(resolved.branch, "main");
    assert_eq!(resolved.sha, base_sha);

    // ANY git revision naming a commit this repository already has is accepted, and every
    // spelling of the same commit resolves to the same sha — so `--base` is documented as
    // taking a sha, a branch, or a tag without any of them being a special case. The
    // annotated tag matters: it is a tag OBJECT, peeled to its commit by `^{commit}`.
    git(&["tag", "lightweight"]);
    git(&["tag", "-a", "annotated", "-m", "annotated", &base_sha]);
    git(&["branch", "elsewhere", &base_sha]);
    let short = git_capture(&tmp, &["rev-parse", "--short", &base_sha]).unwrap().trim().to_string();
    for spec in [short.as_str(), "main", "elsewhere", "annotated", "HEAD~1"] {
      assert_eq!(resolve_run_base(&tmp, spec).unwrap().sha, base_sha, "'{spec}' names the base commit");
    }
    // A lightweight tag on the feature tip is a different commit — still accepted, proving
    // the base need not be an ancestor-shaped "upstream" ref, just a commit that exists.
    assert_ne!(resolve_run_base(&tmp, "lightweight").unwrap().sha, base_sha);

    // An unknown ref is refused by name, before any container work.
    let unknown = resolve_run_base(&tmp, "no-such-ref").unwrap_err();
    assert!(unknown.contains("no-such-ref"), "{unknown}");
    // Nothing is fetched: a ref that exists only on a remote is not a commit here.
    assert!(resolve_run_base(&tmp, "origin/main").is_err());

    let _ = std::fs::remove_dir_all(&tmp);
  }

  #[test]
  fn a_dirty_tree_blocker_names_the_files_and_says_to_commit_them() {
    // The whole point: "1 uncommitted change" with no path is what made people believe a
    // *different skill* was a prerequisite, when all that unblocked the run was committing.
    let one = DefRunBlocker::Dirty(vec!["PR-DESCRIPTION.md".into()]);
    assert_eq!(one.message(), "the working tree has 1 uncommitted change");
    let fixes = one.fixes();
    assert!(fixes.iter().any(|f| f.contains("PR-DESCRIPTION.md")), "names the offending file: {fixes:?}");
    assert!(fixes.iter().any(|f| f.contains("commit or stash")), "says how to clear it: {fixes:?}");
    // No blocker sends a real repository to init-demo-project; only a missing scratch dir
    // mentions it, and then only as the alternative to editing .gitignore.
    assert!(!fixes.iter().any(|f| f.contains("init-demo-project")), "{fixes:?}");
    assert!(!DefRunBlocker::NoCommits.fixes().iter().any(|f| f.contains("init-demo-project")));
    assert!(DefRunBlocker::NoScratchDir.fixes().iter().any(|f| f.contains("init-demo-project")));

    // Long lists are capped, with the remainder summarized rather than dropped silently.
    let many: Vec<String> = (0..DIRTY_PATHS_SHOWN + 3).map(|i| format!("f{i}")).collect();
    let fixes = DefRunBlocker::Dirty(many).fixes();
    assert_eq!(fixes.iter().filter(|f| f.starts_with("uncommitted: ")).count(), DIRTY_PATHS_SHOWN);
    assert!(fixes.iter().any(|f| f.contains("and 3 more")), "{fixes:?}");
    assert_eq!(
      DefRunBlocker::Dirty(vec!["a".into(), "b".into()]).message(),
      "the working tree has 2 uncommitted changes"
    );
  }

  #[test]
  fn run_positional_args_are_profiles() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    // A bare positional after `run` is a profile — `run foo` == `run --profile foo`.
    let c = cli(&["run", "foo"]).unwrap();
    assert!(matches!(c.mode, Mode::Run));
    assert_eq!(c.profile.as_deref(), Some("foo"));
    // Several positionals == a comma list — `run foo bar` == `run --profile foo,bar`.
    assert_eq!(cli(&["run", "foo", "bar"]).unwrap().profile.as_deref(), Some("foo,bar"));
    assert_eq!(cli(&["run", "--profile", "foo,bar"]).unwrap().profile.as_deref(), Some("foo,bar"));
    // `--profile` and positionals combine.
    assert_eq!(cli(&["run", "--profile", "foo", "bar"]).unwrap().profile.as_deref(), Some("foo,bar"));
    // No profile at all → None (the reserved `default` profile runs).
    assert_eq!(cli(&["run"]).unwrap().profile, None);
    // Positional profiles are `run`-only, and never swallow flags or other commands.
    assert!(cli(&["foo"]).is_err(), "a bare token without `run` is an unknown command");
    assert!(cli(&["list", "foo"]).is_err(), "profiles don't apply to `list`");
    assert!(cli(&["run", "--nope"]).is_err(), "an unknown flag after `run` is not a profile");
  }

  #[test]
  fn override_dot_scsh_yml_parses_on_run_list_check() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["run", "code-review", "--override-dot-scsh-yml", "/tmp/bundle/.scsh.yml"]).unwrap();
    assert!(matches!(c.mode, Mode::Run));
    assert_eq!(c.profile.as_deref(), Some("code-review"));
    assert_eq!(c.override_dot_scsh_yml.as_deref(), Some(std::path::Path::new("/tmp/bundle/.scsh.yml")));
    let c = cli(&["check-profile", "code-review", "--override-dot-scsh-yml", "/x.yml"]).unwrap();
    assert!(matches!(c.mode, Mode::CheckProfile));
    assert_eq!(c.override_dot_scsh_yml.as_deref(), Some(std::path::Path::new("/x.yml")));
    assert!(cli(&["run", "--def", "add", "--override-dot-scsh-yml", "/x.yml"]).is_err());
    assert!(cli(&["version", "--override-dot-scsh-yml", "/x.yml"]).is_err());
  }

  #[test]
  fn retries_parses_on_run_only() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["run", "--def", "greet", "--retries", "3"]).unwrap();
    assert!(matches!(c.mode, Mode::Run));
    assert_eq!(c.retries, Some(3));
    assert_eq!(cli(&["run", "--retries", "0"]).unwrap().retries, Some(0), "0 opts out of supervision");
    assert_eq!(cli(&["run"]).unwrap().retries, None, "absent = the daemon's default budget");
    assert!(cli(&["run", "--retries", "many"]).is_err(), "the count is a number");
    assert!(cli(&["list", "--retries", "3"]).is_err(), "run-only flag");
  }

  #[test]
  fn resume_from_parses_on_def_runs_only() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["run", "--def", "greet", "--resume-from", "qtsiuf"]).unwrap();
    assert!(matches!(c.mode, Mode::Run));
    assert_eq!(c.def.as_deref(), Some("greet"));
    assert_eq!(c.resume_from.as_deref(), Some("qtsiuf"));
    assert!(cli(&["run", "--resume-from", "qtsiuf"]).is_err(), "resume without --def has nothing to restore into");
    assert!(cli(&["run", "code-review", "--resume-from", "qtsiuf"]).is_err(), "profiles have no resume");
    assert!(cli(&["run", "--def", "greet", "--resume-from"]).is_err(), "the flag needs a session id");
    assert!(cli(&["run", "--def", "greet", "--resume-from", " "]).is_err(), "…a non-empty one");
  }

  #[test]
  fn workflow_steps_parse_retry_policy_keys() {
    let yml = r#"description: "retry keys"
steps:
  one:
    agent:
      harness: claude
    prompt: do it
    retry_for: 8h
    retry_signature_cap: 2
    inactivity_timeout: 3600
    output:
      done:
        type: bool
"#;
    let def = harness_def::validate("wf", yml, harness_def::DefSource::Builtin).expect("valid def");
    assert_eq!(def.steps[0].retry_for, Some(8 * 3600));
    assert_eq!(def.steps[0].retry_signature_cap, Some(2));
    assert_eq!(def.steps[0].inactivity_timeout, Some(3600));

    let bad = yml.replace("retry_for: 8h", "retry_for: whenever");
    let err = harness_def::validate("wf", &bad, harness_def::DefSource::Builtin).unwrap_err();
    assert!(err.iter().any(|e| e.contains("'steps.one.retry_for' must be a positive duration")), "{err:?}");

    let bad = yml.replace("inactivity_timeout: 3600", "inactivity_timeout: soon");
    let err = harness_def::validate("wf", &bad, harness_def::DefSource::Builtin).unwrap_err();
    assert!(
      err.iter().any(|e| e.contains("'steps.one.inactivity_timeout' must be an integer number of seconds")),
      "{err:?}"
    );
  }

  #[test]
  fn restored_step_result_restores_only_valid_results() {
    let outputs =
      [harness_def::OutputField { name: "grade".into(), ty: harness_def::OutputType::String, choices: vec![] }];
    let contract = WorkflowResultContract { outputs: &outputs, require_do_while_repeat: false };
    let dir = std::env::temp_dir().join(format!("scsh-restore-{}", runtime::random_nonce_6()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("review.json"), r#"{"grade":"good"}"#).unwrap();
    std::fs::write(dir.join("drifted.json"), r#"{"score":5}"#).unwrap();

    // A persisted result that still validates restores: raw JSON + typed outputs + its path.
    let (content, outputs_map, path) = restored_step_result(&dir, "review", contract).expect("valid result restores");
    assert_eq!(content, r#"{"grade":"good"}"#);
    assert_eq!(outputs_map.get("grade").map(String::as_str), Some("good"));
    assert!(path.ends_with("review.json"));
    // Loop iterations look up by their full run id — a different id is a different result.
    assert!(restored_step_result(&dir, "review-while-collect-2", contract).is_none(), "missing iteration re-runs");
    // Schema drift since the old run means the old result no longer satisfies the def: re-run.
    assert!(restored_step_result(&dir, "drifted", contract).is_none(), "an invalid result never restores");
    std::fs::remove_dir_all(&dir).ok();
  }

  #[test]
  fn probe_parses_profiles_json_and_override() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["probe"]).unwrap();
    assert!(matches!(c.mode, Mode::Probe));
    assert_eq!(c.profile, None);
    let c = cli(&["probe", "code-review"]).unwrap();
    assert!(matches!(c.mode, Mode::Probe));
    assert_eq!(c.profile.as_deref(), Some("code-review"));
    let c = cli(&["probe", "code-review", "--json", "--override-dot-scsh-yml", "/x.yml"]).unwrap();
    assert!(c.json);
    assert_eq!(c.override_dot_scsh_yml.as_deref(), Some(std::path::Path::new("/x.yml")));
    // Probe takes several profiles, like run.
    assert_eq!(cli(&["probe", "a", "b"]).unwrap().profile.as_deref(), Some("a,b"));
    // But probe-only flags stay probe-only.
    assert!(cli(&["probe", "--global"]).is_err());
    assert!(cli(&["check-profile", "x", "--json"]).is_err(), "--json doesn't apply to check-profile");
  }

  #[test]
  fn global_flag_parses_on_installskills_and_updateskills_only() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["installskills", "--global"]).unwrap();
    assert!(matches!(c.mode, Mode::InstallSkills));
    assert!(c.global);
    // Flag order doesn't matter, and sources still parse alongside it.
    let c = cli(&["installskills", "--global", "https://example.com/skills.git"]).unwrap();
    assert!(c.global);
    assert_eq!(c.sources, vec!["https://example.com/skills.git".to_string()]);
    let c = cli(&["updateskills", "--global"]).unwrap();
    assert!(matches!(c.mode, Mode::UpdateSkills));
    assert!(c.global);
    // Without the flag, both commands stay repo-scoped.
    assert!(!cli(&["installskills"]).unwrap().global);
    // `--global` belongs to installskills/updateskills alone.
    assert!(cli(&["run", "--global"]).is_err());
    assert!(cli(&["list", "--global"]).is_err());
    assert!(cli(&["version", "--global"]).is_err());
  }

  #[test]
  fn failures_command_parses_filters_and_stats() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["failures"]).unwrap();
    assert!(matches!(c.mode, Mode::Failures));
    assert!(!c.failures.stats);
    let c = cli(&["failures", "--session", "abc123", "--skill", "add", "--reason", "clone_failed"]).unwrap();
    assert_eq!(c.failures.session.as_deref(), Some("abc123"));
    assert_eq!(c.failures.skill.as_deref(), Some("add"));
    assert_eq!(c.failures.reason.as_deref(), Some("clone_failed"));
    let c = cli(&["failures", "--stats", "--last", "0"]).unwrap();
    assert!(c.failures.stats);
    assert_eq!(c.failures.last, Some(0));
    assert!(cli(&["failures", "--last", "many"]).is_err(), "--last needs a number");
    assert!(cli(&["run", "--stats"]).is_err(), "failure filters don't apply to run");
    assert!(cli(&["list", "--session", "abc"]).is_err(), "failure filters don't apply to list");
  }

  #[test]
  fn stats_command_parses_filters() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["stats"]).unwrap();
    assert!(matches!(c.mode, Mode::Stats));
    let c =
      cli(&["stats", "--skill", "conventions-reviewer", "--harness", "codex", "--model", "gpt-5.6-terra"]).unwrap();
    assert_eq!(c.failures.skill.as_deref(), Some("conventions-reviewer"));
    assert_eq!(c.failures.harness.as_deref(), Some("codex"));
    assert_eq!(c.failures.model.as_deref(), Some("gpt-5.6-terra"));
    let c = cli(&["stats", "--profile", "code-review", "--raw", "--last", "10"]).unwrap();
    assert_eq!(c.profile.as_deref(), Some("code-review"));
    assert!(c.failures.raw);
    assert_eq!(c.failures.last, Some(10));
    assert!(cli(&["run", "--raw"]).is_err(), "--raw only applies to stats");
    assert!(cli(&["failures", "--harness", "codex"]).is_err(), "--harness only applies to stats");
    assert!(cli(&["stats", "--reason", "clone_failed"]).is_err(), "--reason only applies to failures");
  }

  #[test]
  fn prune_command_parses_now_flag() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["prune"]).unwrap();
    assert!(matches!(c.mode, Mode::Prune));
    assert!(!c.prune_now);
    assert!(cli(&["prune", "--now"]).unwrap().prune_now);
    assert!(cli(&["run", "--now"]).is_err(), "--now only applies to prune");
  }

  #[test]
  fn gc_command_parses_flags() {
    let cli = |a: &[&str]| parse_cli(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let c = cli(&["gc"]).unwrap();
    assert!(matches!(c.mode, Mode::Gc));
    assert!(!c.gc.apply);
    assert_eq!(c.gc.days, gc::DEFAULT_DAYS);
    assert_eq!(c.gc.keep, gc::DEFAULT_KEEP);
    assert!(!c.gc.legacy);
    let c = cli(&["gc", "--apply", "--days", "7", "--keep", "10", "--legacy"]).unwrap();
    assert!(c.gc.apply);
    assert_eq!(c.gc.days, 7);
    assert_eq!(c.gc.keep, 10);
    assert!(c.gc.legacy);
    assert!(!cli(&["gc", "--dry-run"]).unwrap().gc.apply);
    assert!(cli(&["gc", "--apply", "--dry-run"]).is_err());
    assert!(cli(&["run", "--apply"]).is_err(), "gc flags don't apply to run");
    assert!(cli(&["gc", "--days", "x"]).is_err());
  }

  #[test]
  fn opencode_config_path_prefers_xdg_then_home() {
    let xdg = OsString::from("/cfg");
    let home = OsString::from("/home/u");
    assert_eq!(runtime::opencode_config_dir(Some(&xdg), Some(&home)), Some(PathBuf::from("/cfg/opencode")));
    assert_eq!(runtime::opencode_config_dir(None, Some(&home)), Some(PathBuf::from("/home/u/.config/opencode")));
  }

  #[test]
  fn forward_opencode_copies_auth_and_config_into_the_run_clone() {
    // A fake host opencode home + config, then confirm forward_opencode copies them under the
    // run clone's tmp/ (riding the repo mount), not as separate bind mounts.
    let base = std::env::temp_dir().join(format!("scsh-oc-fwd-{}-{}", std::process::id(), now_secs()));
    let host = base.join("host");
    std::fs::create_dir_all(host.join(".local/share/opencode")).unwrap();
    std::fs::create_dir_all(host.join(".config/opencode")).unwrap();
    std::fs::write(host.join(".local/share/opencode/auth.json"), "{\"k\":1}").unwrap();
    std::fs::write(host.join(".config/opencode/opencode.json"), "{\"provider\":1}").unwrap();
    let run = base.join("run");
    std::fs::create_dir_all(&run).unwrap();
    let prev_home = std::env::var_os("HOME");
    let prev_xdg_d = std::env::var_os("XDG_DATA_HOME");
    let prev_xdg_c = std::env::var_os("XDG_CONFIG_HOME");
    std::env::set_var("HOME", &host);
    std::env::remove_var("XDG_DATA_HOME");
    std::env::remove_var("XDG_CONFIG_HOME");
    let out = forward_opencode(&run);
    match prev_home {
      Some(v) => std::env::set_var("HOME", v),
      None => std::env::remove_var("HOME"),
    }
    if let Some(v) = prev_xdg_d {
      std::env::set_var("XDG_DATA_HOME", v);
    }
    if let Some(v) = prev_xdg_c {
      std::env::set_var("XDG_CONFIG_HOME", v);
    }
    assert!(out.is_some(), "forwarding happened");
    assert!(run.join("tmp/.xdg-data/opencode/auth.json").is_file(), "auth rides the repo mount");
    assert!(run.join("tmp/.config/opencode/opencode.json").is_file(), "config rides the repo mount");
    let _ = std::fs::remove_dir_all(&base);
  }

  // --- commit integration ---------------------------------------------------
  // These exercise integrate_commits against real (synthetic) git repos — no
  // container needed — so the rebase / fallback-branch / run-twice behavior is
  // pinned down in CI. (The full container round-trip is shown in DEMO.md.)

  #[test]
  fn packdiff_failure_detail_reads_machine_mode_errors() {
    let unknown = br#"{
  "UnknownRef": {
    "repo": "/tmp/r",
    "ref": "nope",
    "message": "unknown ref in \"/tmp/r\": nope",
    "stage": "ref",
    "exit_code": 4
  }
}"#;
    assert_eq!(packdiff_failure_detail(unknown).as_deref(), Some("unknown ref in \"/tmp/r\": nope"));
    assert_eq!(
      packdiff_failure_detail(br#"{ "NotAGitRepository": { "stage": "repo", "exit_code": 3 } }"#).as_deref(),
      Some("NotAGitRepository")
    );
    assert_eq!(packdiff_failure_detail(br#"{ "Packed": { "out": "x.html" } }"#), None);
    assert_eq!(packdiff_failure_detail(b""), None);
  }

  #[test]
  fn cached_skill_run_carries_result_json_for_workflow_outputs() {
    // A workflow consumes validated outputs after each wave. A cache hit must carry both the
    // original JSON and the already-validated fields, or the DAG cannot bind downstream inputs.
    let outputs = std::collections::HashMap::from([("sum".to_string(), "5".to_string())]);
    let run = SkillRun::cached(None, r#"{"sum":5}"#.into(), Some(outputs));
    assert!(run.ok && run.cached);
    assert_eq!(run.result_content.as_deref(), Some(r#"{"sum":5}"#));
    assert_eq!(run.workflow_outputs.as_ref().and_then(|fields| fields.get("sum")).map(String::as_str), Some("5"));
  }

  use std::sync::atomic::{AtomicUsize, Ordering};
  static MT: AtomicUsize = AtomicUsize::new(0);

  fn mt_dir(tag: &str) -> PathBuf {
    let n = MT.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("scsh-mt-{tag}-{}-{}-{n}", std::process::id(), now_secs()));
    std::fs::create_dir_all(&d).unwrap();
    d
  }

  fn g(dir: &Path, args: &[&str]) {
    assert!(git_status_ok(dir, args), "git {args:?} should succeed in {}", dir.display());
  }

  fn head(dir: &Path) -> String {
    git_capture(dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
  }

  /// A fresh repo with one `base` commit and a local identity.
  fn repo(tag: &str) -> PathBuf {
    let d = mt_dir(tag);
    g(&d, &["init", "-q", "."]);
    g(&d, &["config", "user.email", "t@e.st"]);
    g(&d, &["config", "user.name", "tester"]);
    std::fs::write(d.join("README"), "base\n").unwrap();
    g(&d, &["add", "-A"]);
    g(&d, &["commit", "-qm", "base"]);
    d
  }

  /// Clone `src` and commit a change in the clone (mimicking a commit-enabled skill).
  fn clone_and_commit(src: &Path, tag: &str, file: &str, contents: &str, msg: &str) -> PathBuf {
    let d = mt_dir(tag);
    assert!(
      git_command()
        .args(["clone", "-q", &src.to_string_lossy(), &d.to_string_lossy()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false),
      "clone should succeed"
    );
    set_clone_identity(&d, None);
    std::fs::write(d.join(file), contents).unwrap();
    g(&d, &["add", "-A"]);
    g(&d, &["commit", "-qm", msg]);
    d
  }

  #[test]
  fn incoming_branch_name_is_distinct_and_descriptive() {
    let n = incoming_branch_name("add", "20231114-221320", "abcdef1234567");
    assert_eq!(n, "scsh/incoming/add-20231114-221320-utc-abcdef1");
    // A messy skill name is sanitized into a valid ref component.
    assert!(incoming_branch_name("My Skill!", "S", "deadbeef").starts_with("scsh/incoming/my-skill-S-utc-"));
  }

  #[test]
  fn integrate_rebases_clean_commits_onto_the_branch() {
    let caller = repo("clean-caller");
    let base = head(&caller);
    let clone = clone_and_commit(&caller, "clean-clone", "foo.txt", "hi\n", "add foo");

    let outcome = integrate_commits(&caller, &clone, &base, "add", "STAMP").unwrap();
    let range = match outcome {
      Some(Integration::Applied { count: 1, range }) => range,
      other => panic!("expected 1 applied commit, got {:?}", other.is_some()),
    };
    // The reported range spans exactly the integrated commit ON THE CALLER's branch — the
    // pair pack_step_diff hands to packdiff: from the pre-integration HEAD to the new HEAD.
    let (from, to) = range.expect("Applied carries the caller-repo range");
    assert_eq!(from, base);
    assert_eq!(to, head(&caller));
    assert_eq!(git_capture(&caller, &["rev-list", "--count", &format!("{from}..{to}")]).unwrap().trim(), "1");
    // The file is now committed on the caller's branch, the tree is clean, and HEAD
    // advanced by exactly one commit.
    assert_eq!(std::fs::read_to_string(caller.join("foo.txt")).unwrap(), "hi\n");
    assert_eq!(git_capture(&caller, &["status", "--porcelain"]).unwrap().trim(), "");
    assert_eq!(git_capture(&caller, &["rev-list", "--count", &format!("{base}..HEAD")]).unwrap().trim(), "1");
    // The brought-in commit keeps the deliberately recognizable bot author (the
    // "not-for-pushing" tripwire); the committer is the caller.
    assert_eq!(git_capture(&caller, &["log", "-1", "--format=%ae"]).unwrap().trim(), SCSH_COMMIT_EMAIL);
    assert_eq!(git_capture(&caller, &["log", "-1", "--format=%an"]).unwrap().trim(), SCSH_COMMIT_NAME);
  }

  #[test]
  fn integrate_rebases_second_skill_onto_advanced_head() {
    // Two skills both branched from the same base; the second must rebase onto the
    // HEAD the first advanced to (not fast-forward), ending with BOTH files.
    let caller = repo("two-caller");
    let base = head(&caller);
    let c1 = clone_and_commit(&caller, "two-c1", "a.txt", "A\n", "add a");
    let c2 = clone_and_commit(&caller, "two-c2", "b.txt", "B\n", "add b");

    assert!(matches!(integrate_commits(&caller, &c1, &base, "s1", "S").unwrap(), Some(Integration::Applied { .. })));
    assert!(matches!(integrate_commits(&caller, &c2, &base, "s2", "S").unwrap(), Some(Integration::Applied { .. })));
    assert!(caller.join("a.txt").is_file() && caller.join("b.txt").is_file(), "both skills' files land");
    assert_eq!(git_capture(&caller, &["rev-list", "--count", &format!("{base}..HEAD")]).unwrap().trim(), "2");
  }

  #[test]
  fn integrate_saves_conflicting_commits_to_a_branch() {
    let caller = repo("conf-caller");
    let base = head(&caller);
    // Both skills are cloned up front from the SAME base (as scsh does), and both add
    // the same file with different content.
    let c1 = clone_and_commit(&caller, "conf-c1", "shared.txt", "one\n", "shared one");
    let c2 = clone_and_commit(&caller, "conf-c2", "shared.txt", "two\n", "shared two");
    // The first applies cleanly; cherry-picking the second onto the now-advanced caller
    // (which already has shared.txt="one") is an add/add conflict.
    integrate_commits(&caller, &c1, &base, "s1", "S").unwrap();
    let outcome = integrate_commits(&caller, &c2, &base, "s2", "S").unwrap();
    let (branch, range) = match outcome {
      Some(Integration::Saved { branch, count: 1, range }) => (branch, range),
      other => panic!("expected the conflicting commit to be saved to a branch, got {:?}", other.is_some()),
    };
    // Saved commits still get a packable range: base..tip resolves in the caller repo
    // because the objects were fetched, even though the branch wasn't advanced.
    let (from, to) = range.expect("Saved carries the base..tip range");
    assert_eq!(from, base);
    assert_eq!(git_capture(&caller, &["rev-list", "--count", &format!("{from}..{to}")]).unwrap().trim(), "1");
    // The caller's branch is untouched (still "one"); the fallback branch exists and
    // carries the skill's commit.
    assert_eq!(std::fs::read_to_string(caller.join("shared.txt")).unwrap(), "one\n");
    assert_eq!(git_capture(&caller, &["status", "--porcelain"]).unwrap().trim(), "", "no half-applied cherry-pick");
    assert!(branch.starts_with("scsh/incoming/s2-"));
    assert_eq!(git_capture(&caller, &["cat-file", "-t", &branch]).unwrap().trim(), "commit");
  }

  #[test]
  fn saved_rewrite_is_the_revision_seen_by_the_next_workflow_wave() {
    let caller = repo("workflow-rewrite-caller");
    std::fs::write(caller.join("feature.txt"), "first version\n").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "feature"]);
    let base = head(&caller);

    let rewritten = mt_dir("workflow-rewrite-run");
    assert!(git_command()
      .args(["clone", "-q", &caller.to_string_lossy(), &rewritten.to_string_lossy()])
      .status()
      .unwrap()
      .success());
    set_clone_identity(&rewritten, None);
    g(&rewritten, &["reset", "--hard", "HEAD~1"]);
    std::fs::write(rewritten.join("feature.txt"), "authoritative rewrite\n").unwrap();
    g(&rewritten, &["add", "-A"]);
    g(&rewritten, &["commit", "-qm", "rewrite feature"]);

    let outcome = integrate_commits(&caller, &rewritten, &base, "fix", "S").unwrap();
    let (branch, tip) = match outcome {
      Some(Integration::Saved { branch, range: Some((_, tip)), .. }) => (branch, tip),
      _ => panic!("rewritten history should be preserved on an incoming branch"),
    };
    assert_eq!(head(&caller), base, "the caller branch remains untouched");
    assert_eq!(git_capture(&caller, &["rev-parse", &branch]).unwrap().trim(), tip);

    let next = mt_dir("workflow-next-wave");
    assert!(git_command()
      .args(["clone", "-q", &caller.to_string_lossy(), &next.to_string_lossy()])
      .status()
      .unwrap()
      .success());
    checkout_workflow_revision(&next, &tip).unwrap();
    assert_eq!(std::fs::read_to_string(next.join("feature.txt")).unwrap(), "authoritative rewrite\n");

    let bare = mt_dir("workflow-transport").join("transport.git");
    runtime::push_transport_refs(&caller, &bare).unwrap();
    select_transport_workflow_revision(&bare, &tip).unwrap();
    assert_eq!(git_capture(&bare, &["rev-parse", "HEAD"]).unwrap().trim(), tip);
  }

  #[test]
  fn integrate_is_a_noop_when_the_skill_added_no_commits() {
    let caller = repo("noop-caller");
    let base = head(&caller);
    let d = mt_dir("noop-clone");
    assert!(git_command()
      .args(["clone", "-q", &caller.to_string_lossy(), &d.to_string_lossy()])
      .status()
      .unwrap()
      .success());
    // No commit made in the clone → nothing to bring back.
    assert!(integrate_commits(&caller, &d, &base, "add", "S").unwrap().is_none());
  }

  #[test]
  fn commits_are_a_side_effect_run_twice_adds_twice() {
    // Models a skill that appends a line and commits, run on two consecutive
    // invocations (each captures its own base = the current HEAD). The result is two
    // commits and a two-line file — adding a commit is a side effect, not deduped.
    let caller = repo("twice-caller");
    let base1 = head(&caller);
    let r1 = clone_and_commit(&caller, "twice-r1", "log.txt", "x\n", "log x");
    integrate_commits(&caller, &r1, &base1, "add", "S").unwrap();

    let base2 = head(&caller); // the next run's base is the now-advanced HEAD
    let r2 = clone_and_commit(&caller, "twice-r2", "log.txt", "x\nx\n", "log x again");
    integrate_commits(&caller, &r2, &base2, "add", "S").unwrap();

    assert_eq!(std::fs::read_to_string(caller.join("log.txt")).unwrap(), "x\nx\n");
    assert_eq!(git_capture(&caller, &["rev-list", "--count", &format!("{base1}..HEAD")]).unwrap().trim(), "2");
  }

  // --- result cache ---------------------------------------------------------

  fn mk_inv(name: &str) -> config::ResolvedInvocation {
    config::ResolvedInvocation {
      name: name.into(),
      skill_source: name.into(),
      harness: config::Harness::Opencode,
      model: None,
      effort: None,
      timeout: None,
      inactivity_timeout: None,
      retry_for: None,
      retry_signature_cap: None,
      env: Vec::new(),
      profile: None,
      commits: false,
      commit_identity: None,
      result: "tmp/r.json".into(),
      terminal: config::Terminal::default(),
      delivery: config::SkillDelivery::Repo,
      artifacts: Vec::new(),
    }
  }

  #[test]
  fn cache_key_is_deterministic_and_sensitive() {
    let caller = repo("ck");
    std::fs::create_dir_all(caller.join(".skills/add")).unwrap();
    std::fs::write(caller.join(".skills/add/SKILL.md"), "name: add\nbody\n").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "skill"]);
    let s = mk_inv("add");
    let env = vec![("A".to_string(), "2".to_string()), ("B".to_string(), "3".to_string())];

    let k1 = cache_key(&caller, &s, &env).unwrap();
    assert_eq!(k1.len(), 64, "key is a sha256 hex digest");
    // Same inputs => same key; env order doesn't matter (it's sorted).
    assert_eq!(cache_key(&caller, &s, &env).unwrap(), k1);
    let env_rev = vec![("B".to_string(), "3".to_string()), ("A".to_string(), "2".to_string())];
    assert_eq!(cache_key(&caller, &s, &env_rev).unwrap(), k1);
    // Different env => different key.
    let env2 = vec![("A".to_string(), "9".to_string()), ("B".to_string(), "3".to_string())];
    assert_ne!(cache_key(&caller, &s, &env2).unwrap(), k1);

    // An input variable ALWAYS drives the key, even alongside a differing SCSH_RESULT — and
    // SCSH_RESULT alone (the output path, e.g. a workflow's per-session dir) must NOT change it.
    let env_a2 = vec![("A".into(), "2".to_string()), ("SCSH_RESULT".into(), "tmp/scsh/aaa/x.json".to_string())];
    let env_a2b = vec![("A".into(), "2".to_string()), ("SCSH_RESULT".into(), "tmp/scsh/zzz/x.json".to_string())];
    let env_a7 = vec![("A".into(), "7".to_string()), ("SCSH_RESULT".into(), "tmp/scsh/aaa/x.json".to_string())];
    assert_eq!(
      cache_key(&caller, &s, &env_a2).unwrap(),
      cache_key(&caller, &s, &env_a2b).unwrap(),
      "output path is not part of the key"
    );
    assert_ne!(
      cache_key(&caller, &s, &env_a2).unwrap(),
      cache_key(&caller, &s, &env_a7).unwrap(),
      "an input value changes the key"
    );

    // A definition/workflow body drives the key (it isn't a caller .skills/ file).
    let mut with_body = s.clone();
    with_body.delivery = config::SkillDelivery::DirectPrompt("do X".into());
    let mut with_body2 = s.clone();
    with_body2.delivery = config::SkillDelivery::DirectPrompt("do Y".into());
    assert_ne!(
      cache_key(&caller, &with_body, &env).unwrap(),
      cache_key(&caller, &with_body2, &env).unwrap(),
      "body change busts the cache"
    );

    // A committed change to repo content => different key (the HEAD tree changed).
    std::fs::write(caller.join("other.txt"), "x").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "change"]);
    assert_ne!(cache_key(&caller, &s, &env).unwrap(), k1);

    // Editing the skill body => different key too.
    let before_skill_edit = cache_key(&caller, &s, &env).unwrap();
    std::fs::write(caller.join(".skills/add/SKILL.md"), "name: add\nNEW body\n").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "edit skill"]);
    assert_ne!(cache_key(&caller, &s, &env).unwrap(), before_skill_edit);
  }

  #[test]
  fn cache_store_lookup_and_restore_roundtrip() {
    let caller = repo("cs");
    let key = "deadbeef";
    assert!(cache_lookup(&caller, key).is_none(), "empty cache misses");

    let result = r#"{"result": "2 + 3 = 5"}"#;
    // Store with a duration and a cast recording, so a hit can reproduce them.
    let cast_src = caller.join("orig.cast");
    std::fs::write(&cast_src, "cast-bytes").unwrap();
    let chapters_src = caller.join("orig.chapters.json");
    std::fs::write(&chapters_src, r#"{"summary":"hi","chapters":[{"t":1.0,"title":"start"}]}"#).unwrap();
    cache_store(&caller, key, result, None, Some(12.5), Some(&cast_src));
    // Stored under the repo's gitignored tmp/.sccache/<key>.json, and reads back.
    assert!(caller.join("tmp/.sccache").join(format!("{key}.json")).is_file());
    let entry = cache_lookup(&caller, key).expect("hit");
    assert_eq!(entry.result, result);
    assert!(entry.commits.is_none(), "no commits journaled for a non-committing skill");
    assert_eq!(entry.elapsed, Some(12.5), "the original duration round-trips");
    assert!(entry.cached_at.is_some(), "cached_at is stamped on store");
    let cast = entry.cast.expect("the cast was cached");
    assert_eq!(std::fs::read_to_string(&cast).unwrap(), "cast-bytes", "the recording round-trips");
    let chapters = entry.chapters.expect("chapters were cached with the cast");
    assert!(std::fs::read_to_string(&chapters).unwrap().contains("\"summary\""), "chapters round-trip");
    // Durable cast remembers its cache key so a later annotate can attach chapters.
    assert_eq!(std::fs::read_to_string(format!("{}.sccache-key", cast_src.display())).unwrap().trim(), key);

    // Restoring writes the result file (creating tmp/), exactly as a real run would have.
    restore_cached_result(&caller, "tmp/add_result.json", result).unwrap();
    assert_eq!(std::fs::read_to_string(caller.join("tmp/add_result.json")).unwrap(), result);
    // And the human message is recoverable from the restored content.
    assert_eq!(json::message(result).as_deref(), Some("2 + 3 = 5"));

    // A commit-enabled skill journals its commits (a patch mbox); they round-trip and a
    // multi-line patch with quotes survives the JSON quoting.
    let patch = r#"From abc Mon Sep 17 00:00:00 2001
Subject: [PATCH] add: 2 + 3 = 5

"diff" body
"#;
    cache_store(&caller, "withcommit", result, Some(patch), None, None);
    let e2 = cache_lookup(&caller, "withcommit").expect("hit");
    assert_eq!(e2.result, result);
    assert_eq!(e2.commits.as_deref(), Some(patch));
    assert!(e2.elapsed.is_none() && e2.cast.is_none(), "optional fields absent when not stored");
    assert!(e2.cached_at.is_some(), "cached_at still stamped without a cast");
  }

  #[test]
  fn cache_attach_chapters_copies_sidecar_after_annotate() {
    let caller = repo("chap-cache");
    let key = "cafechap";
    let cast_src = caller.join("run.cast");
    std::fs::write(&cast_src, "cast").unwrap();
    // Store without chapters (the usual order: cache_store finishes before annotate).
    cache_store(&caller, key, r#"{"ok":true}"#, None, Some(1.0), Some(&cast_src));
    assert!(cache_lookup(&caller, key).unwrap().chapters.is_none());
    // Annotate lands the sidecar next to the durable cast.
    std::fs::write(caller.join("run.chapters.json"), r#"{"summary":"done","chapters":[{"t":0.5,"title":"hi"}]}"#)
      .unwrap();
    cache_attach_chapters(&caller, &cast_src);
    let entry = cache_lookup(&caller, key).expect("hit");
    let chapters = entry.chapters.expect("chapters attached into the cache");
    assert!(std::fs::read_to_string(chapters).unwrap().contains("done"));
  }

  #[test]
  fn format_cached_at_is_human_utc() {
    assert_eq!(format_cached_at(0), "1970-01-01 00:00 UTC");
    assert_eq!(format_cached_at(1_700_000_000), "2023-11-14 22:13 UTC");
  }

  #[test]
  fn cache_hit_labels_source_duration_as_provenance() {
    assert_eq!(
      cache_hit_provenance(Some(1_700_000_000), Some(480.946)),
      "cached · 2023-11-14 22:13 UTC · source run took 8m 0s"
    );
    assert_eq!(cache_hit_provenance(None, None), "cached");
  }

  #[test]
  fn cached_commits_are_replayed() {
    let caller = repo("replay");
    let base = git_capture(&caller, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
    // Make a commit, capture it as a patch (what cache_store journals), then revert to base.
    std::fs::write(caller.join("note.txt"), "hi\n").unwrap();
    g(&caller, &["add", "-A"]);
    g(&caller, &["commit", "-qm", "add note"]);
    let patch = commit_patch(&caller, &base).expect("a commit patch");
    g(&caller, &["reset", "--hard", &base]);
    assert!(!caller.join("note.txt").exists(), "reverted to base");
    // Replaying the journaled patch (what a cache HIT does) brings the commit back.
    let res = apply_cached_commits(&caller, &patch, "demo", "20260101-000000");
    assert!(
      matches!(res, Ok(Some(Integration::Applied { count: 1, range: Some(_) }))),
      "expected Applied{{count:1}} with a packable range"
    );
    assert!(caller.join("note.txt").exists(), "replayed file is present");
    assert_eq!(git_capture(&caller, &["log", "-1", "--format=%s"]).unwrap().trim(), "add note");
    assert_ne!(git_capture(&caller, &["rev-parse", "HEAD"]).unwrap().trim(), base, "HEAD advanced past base");
  }

  #[test]
  fn step_invocation_routes_artifacts_into_the_session_dir_and_bypasses_the_cache() {
    let step = harness_def::Step {
      id: "summarize".into(),
      agent: harness_def::StepAgent { harness: config::Harness::Grok, model: Some("grok-4.5".into()), effort: None },
      task: harness_def::StepTask::Prompt("p".into()),
      inputs: Vec::new(),
      outputs: Vec::new(),
      commit_identity: harness_def::CommitIdentity::Notes,
      when: None,
      needs: vec!["add".into()],
      artifacts: vec!["summary.txt".into()],
      commits: false,
      repeat: None,
      retry_for: None,
      retry_signature_cap: None,
      inactivity_timeout: Some(3600),
      do_while: None,
      break_loop: false,
      max_iterations: None,
    };
    let inv = step_invocation(&step, "summarize", "tmp/scsh/abcdef", Vec::new(), None);
    // The artifact lands beside the step's result, inside the caller's session scratch dir.
    assert_eq!(inv.result, "tmp/scsh/abcdef/summarize.json");
    assert_eq!(inv.artifacts, vec!["tmp/scsh/abcdef/summary.txt".to_string()]);
    // The step's own novelty window rides into the invocation — the watchdog reads it from there.
    assert_eq!(inv.inactivity_timeout, Some(3600));
    // Side files are not journaled in the cache, so artifact steps must always run live.
    let caller = repo("artifacts-nocache");
    assert!(cache_key(&caller, &inv, &[]).is_none(), "artifact steps must bypass the cache");
  }

  #[test]
  fn resolve_input_falls_back_to_the_previous_loop_iteration() {
    // The loop-carried channel: a do-while body step reads the loop end's PREVIOUS-iteration
    // output. Empty on round one, populated once the loop repeats, and current state wins once
    // the step has a value THIS round. Exercised live by the gorgeous-pipeline builtin, whose
    // `decide` reads `collect.feedback` from the round before.
    let (_, src) = harness_def::builtin_defs().into_iter().find(|(n, _)| *n == "gorgeous-pipeline").unwrap();
    let def = harness_def::validate("gorgeous-pipeline", src, harness_def::DefSource::Builtin).unwrap();
    let feedback = harness_def::Ref::StepField { step: "collect".into(), field: "feedback".into() };
    let mut state = std::collections::HashMap::new();
    let mut loop_prev = std::collections::HashMap::new();
    assert_eq!(resolve_input(&feedback, &def, &state, &loop_prev), "", "round one: no feedback yet");
    loop_prev.insert(
      "collect".to_string(),
      StepState { skipped: false, outputs: [("feedback".to_string(), "add tests".to_string())].into() },
    );
    assert_eq!(resolve_input(&feedback, &def, &state, &loop_prev), "add tests", "round two reads the saved round");
    state.insert(
      "collect".to_string(),
      StepState { skipped: false, outputs: [("feedback".to_string(), "current".to_string())].into() },
    );
    assert_eq!(resolve_input(&feedback, &def, &state, &loop_prev), "current", "live state beats the carried value");
  }

  #[test]
  fn loop_iterations_blank_round_zero_inputs_and_carry_the_iteration_number() {
    let (_, src) = harness_def::builtin_defs().into_iter().find(|(n, _)| *n == "gorgeous-pipeline").unwrap();
    let def = harness_def::validate("gorgeous-pipeline", src, harness_def::DefSource::Builtin).unwrap();
    let decide = def.steps.iter().find(|s| s.id == "decide").unwrap();
    let end = def.steps.iter().find(|s| s.id == "collect").unwrap();
    let body: Vec<String> = harness_def::do_while_body(&def.steps, end).into_iter().map(str::to_string).collect();
    let mut state = std::collections::HashMap::new();
    state.insert(
      "initial_conventions_opus".to_string(),
      StepState { skipped: false, outputs: [("grade".to_string(), "excellent".to_string())].into() },
    );
    let mut loop_prev = std::collections::HashMap::new();
    loop_prev.insert(
      "collect".to_string(),
      StepState { skipped: false, outputs: [("feedback".to_string(), "round report".to_string())].into() },
    );
    let get = |v: &[(String, String)], k: &str| v.iter().find(|(n, _)| n == k).map(|(_, x)| x.clone()).unwrap();

    let one = step_loop_inputs(decide, &def, &state, &loop_prev, Some(&body), 1);
    assert_eq!(get(&one, "INITIAL_CONVENTIONS_OPUS"), "excellent", "iteration 1 reads the round-0 reviews");
    assert_eq!(get(&one, "SCSH_LOOP_ITERATION"), "1");

    let two = step_loop_inputs(decide, &def, &state, &loop_prev, Some(&body), 2);
    assert_eq!(get(&two, "INITIAL_CONVENTIONS_OPUS"), "", "round-0 reviews are history from iteration 2 on");
    assert_eq!(get(&two, "PREVIOUS_FEEDBACK"), "round report", "the loop-carried channel stays live");
    assert_eq!(get(&two, "SCSH_LOOP_ITERATION"), "2");

    let outside = step_loop_inputs(decide, &def, &state, &loop_prev, None, 2);
    assert!(!outside.iter().any(|(n, _)| n == "SCSH_LOOP_ITERATION"), "non-loop steps see no iteration variable");
  }

  #[test]
  fn inherited_git_dir_never_hijacks_explicit_repo_targets() {
    // git exports GIT_DIR to hook and `rebase --exec` children, and an inherited GIT_DIR
    // overrides `-C` discovery — this is how a test suite run under `git rebase --exec`
    // once committed fixture history into the enclosing checkout.
    let d = repo("gitdir");
    let expected = git_capture(&d, &["rev-parse", "--absolute-git-dir"]).unwrap();
    std::env::set_var("GIT_DIR", "/definitely/not/a/repo");
    let seen = git_capture(&d, &["rev-parse", "--absolute-git-dir"]);
    std::env::remove_var("GIT_DIR");
    assert_eq!(seen.as_deref().map(str::trim), Some(expected.trim()), "-C target wins over inherited GIT_DIR");
  }

  #[test]
  fn clone_identity_defaults_to_the_bot_and_honors_the_runner_override() {
    let d = repo("cident");
    set_clone_identity(&d, None);
    assert_eq!(git_capture(&d, &["config", "user.email"]).unwrap().trim(), SCSH_COMMIT_EMAIL);
    let jane = ("Jane Dev".to_string(), "jane@example.com".to_string());
    set_clone_identity(&d, Some(&jane));
    assert_eq!(git_capture(&d, &["config", "user.name"]).unwrap().trim(), "Jane Dev");
    assert_eq!(git_capture(&d, &["config", "user.email"]).unwrap().trim(), "jane@example.com");
    // And the runner identity reads back exactly what the repo's own config declares.
    assert_eq!(runner_commit_identity(&d), Some(jane));
  }

  #[test]
  fn global_install_lands_in_the_harness_skills_dir_on_both_transports() {
    let base = std::env::temp_dir().join(format!("scsh-global-skill-{}", runtime::random_nonce_6()));
    let inv = |harness: config::Harness| config::ResolvedInvocation {
      name: "greet".into(),
      skill_source: "greet".into(),
      harness,
      model: None,
      effort: None,
      timeout: None,
      inactivity_timeout: None,
      retry_for: None,
      retry_signature_cap: None,
      env: Vec::new(),
      profile: None,
      commits: false,
      commit_identity: None,
      result: "tmp/greet.json".into(),
      terminal: config::Terminal::default(),
      delivery: config::SkillDelivery::GlobalInstall("# greet\nsay hi\n".into()),
      artifacts: Vec::new(),
    };
    // claude: the CLI's user-level skills dir (under CLAUDE_CONFIG_DIR). The write must land
    // identically with and without the git transport — tmp/ is mounted on both.
    for git_transport in [false, true] {
      let run_dir = base.join(format!("claude-{git_transport}"));
      std::fs::create_dir_all(&run_dir).unwrap();
      materialize_skill_body(&run_dir, git_transport, &inv(config::Harness::Claude)).unwrap();
      let installed = run_dir.join("tmp/.claude-auth/.claude/skills/greet/SKILL.md");
      assert!(installed.is_file(), "git_transport={git_transport}");
      assert!(!run_dir.join(".skills").exists(), "the checkout never contains a global skill");
    }
    // A harness without native discovery gets the neutral container path.
    let run_dir = base.join("opencode");
    std::fs::create_dir_all(&run_dir).unwrap();
    materialize_skill_body(&run_dir, false, &inv(config::Harness::Opencode)).unwrap();
    assert!(run_dir.join("tmp/.scsh-skills/greet/SKILL.md").is_file());
    let _ = std::fs::remove_dir_all(&base);
  }

  #[test]
  fn ensure_tmp_gitignored_adds_rule_and_creates_dir() {
    let root = repo("tmp-ensure");
    assert!(!root.join("tmp").is_dir());
    assert!(!tmp_is_gitignored(&root));
    assert!(ensure_tmp_gitignored(&root).unwrap());
    assert!(tmp_is_gitignored(&root));
    assert!(root.join("tmp").is_dir());
    // Second call is a no-op for the rule, but still keeps the dir.
    assert!(!ensure_tmp_gitignored(&root).unwrap());
    assert!(root.join("tmp").is_dir());
  }
}
