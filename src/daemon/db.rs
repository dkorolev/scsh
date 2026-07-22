//! The daemon's persistent store, backed by an embedded redb database at
//! `~/.scsh/daemon-<port>.redb`.
//!
//! Only the session browser daemon opens it (redb allows a single process to hold a DB at a
//! time). Each session is one row — `session_id -> session JSON` — so a mutation writes just
//! that session, not a rewrite of the whole store. This replaces the old scheme that
//! serialized every session into one growing JSON file and wrote it in full on every tick.
//!
//! Serialization reuses the hand-rolled JSON in [`super::jsonio`] (no serde), so the on-disk
//! shape matches the WebSocket payload and the model stays the single source of truth.

use std::collections::BTreeMap;

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use super::jsonio;
use super::model::Session;

/// `session_id -> session JSON (UTF-8 bytes)`.
const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");

/// A handle to the daemon's redb store. Cheap to clone-free share via `&self` — redb
/// serializes its own access, so no external lock is needed.
pub struct StoreDb {
  /// The open redb database (holds an exclusive OS lock on the file for this process's life).
  db: Database,
}

impl StoreDb {
  /// Open (creating if needed) the store at `~/.scsh/daemon-<port>.redb`.
  pub fn open(port: u16) -> Result<StoreDb, String> {
    Self::open_path(&super::paths::store_db_file(port))
  }

  /// Open (creating if needed) a store at an explicit path, ensuring its table exists so a
  /// brand-new DB reads cleanly. Errors carry the path for a legible failure.
  pub fn open_path(path: &std::path::Path) -> Result<StoreDb, String> {
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let db = Database::create(path).map_err(|e| format!("open redb {}: {e}", path.display()))?;
    let txn = db.begin_write().map_err(|e| e.to_string())?;
    {
      let _ = txn.open_table(SESSIONS).map_err(|e| e.to_string())?;
    }
    txn.commit().map_err(|e| e.to_string())?;
    Ok(StoreDb { db })
  }

  /// Load every persisted session (`id -> Session`). Malformed rows are skipped rather than
  /// failing the load. Production startup uses [`StoreDb::load_working_set`]; this full read
  /// remains for tests asserting on the complete archive.
  #[cfg(test)]
  pub fn load_sessions(&self) -> BTreeMap<String, Session> {
    let mut out = BTreeMap::new();
    let Ok(txn) = self.db.begin_read() else { return out };
    let Ok(table) = txn.open_table(SESSIONS) else { return out };
    let Ok(iter) = table.iter() else { return out };
    for (k, v) in iter.flatten() {
      if let Ok(session) = jsonio::parse_session_json(&String::from_utf8_lossy(v.value())) {
        out.insert(k.value().to_string(), session);
      }
    }
    out
  }

  /// One session by id, parsed from its stored row. This is the read the API and page
  /// handlers fall back to when a session has been evicted from the in-memory working set:
  /// eviction is an archival move, not a deletion, so the row is still here.
  pub fn get(&self, id: &str) -> Option<Session> {
    let txn = self.db.begin_read().ok()?;
    let table = txn.open_table(SESSIONS).ok()?;
    let row = table.get(id).ok()??;
    jsonio::parse_session_json(&String::from_utf8_lossy(row.value())).ok()
  }

  /// One-transaction sync: write each `(id, json)` in `dirty`. Rows are never deleted here —
  /// a session evicted from the in-memory cap keeps its last-written row as its archive
  /// (served read-only via [`StoreDb::get`]); the archive itself is bounded once, at
  /// startup, by [`StoreDb::load_working_set`].
  pub fn sync(&self, dirty: &[(String, String)]) -> Result<(), String> {
    let txn = self.db.begin_write().map_err(|e| e.to_string())?;
    {
      let mut table = txn.open_table(SESSIONS).map_err(|e| e.to_string())?;
      for (id, json) in dirty {
        table.insert(id.as_str(), json.as_bytes()).map_err(|e| e.to_string())?;
      }
    }
    txn.commit().map_err(|e| e.to_string())?;
    Ok(())
  }

  /// The daemon's startup load: every RUNNING session plus the newest `mem_cap` finished
  /// ones become the in-memory working set, and — in the same pass — finished rows beyond
  /// `disk_cap` (oldest first) are deleted, which is the archive's only pruning point.
  /// Memory stays bounded while reading: each row is parsed once, and only the current
  /// working-set candidates are held (a `BTreeMap` keyed by start time, oldest popped as
  /// newer sessions displace it).
  pub fn load_working_set(&self, mem_cap: usize, disk_cap: usize, now: u64) -> BTreeMap<String, Session> {
    let mut running: Vec<(String, Session)> = Vec::new();
    // Finished working-set candidates, keyed `(started_at, id)` so pop_first drops the oldest.
    let mut newest: BTreeMap<(u64, String), Session> = BTreeMap::new();
    // Every finished row's `(started_at, id)`, for the disk-cap decision below.
    let mut finished: Vec<(u64, String)> = Vec::new();
    {
      let Ok(txn) = self.db.begin_read() else { return BTreeMap::new() };
      let Ok(table) = txn.open_table(SESSIONS) else { return BTreeMap::new() };
      let Ok(iter) = table.iter() else { return BTreeMap::new() };
      for (k, v) in iter.flatten() {
        let id = k.value().to_string();
        let Ok(session) = jsonio::parse_session_json(&String::from_utf8_lossy(v.value())) else { continue };
        if session.lifecycle_status(now) == super::model::SessionLifecycle::Running {
          // A running session is never evicted from memory, whatever its age (the same
          // exemption trim_sessions_to_cap applies): its client may re-attach after this
          // very restart.
          running.push((id, session));
          continue;
        }
        finished.push((session.started_at, id.clone()));
        newest.insert((session.started_at, id), session);
        if newest.len() > mem_cap {
          newest.pop_first();
        }
      }
    }
    if finished.len() > disk_cap {
      finished.sort_unstable();
      let stale: Vec<&(u64, String)> = finished.iter().take(finished.len() - disk_cap).collect();
      if let Ok(txn) = self.db.begin_write() {
        if let Ok(mut table) = txn.open_table(SESSIONS) {
          for (_, id) in &stale {
            let _ = table.remove(id.as_str());
          }
        }
        let _ = txn.commit();
      }
    }
    let mut out: BTreeMap<String, Session> = running.into_iter().collect();
    out.extend(newest.into_iter().map(|((_, id), s)| (id, s)));
    out
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::daemon::model::{DaemonMode, ProcKind, ProcRecord, ProcStatus, Session, Store};

  #[test]
  fn load_szwwuv_backfills_graph_needs_when_present_in_store() {
    let path = super::super::paths::store_db_file(7274);
    if !path.exists() {
      return;
    }
    let db = match StoreDb::open_path(&path) {
      Ok(db) => db,
      Err(e) if e.contains("Cannot acquire lock") => return,
      Err(e) => panic!("open store: {e}"),
    };
    let sessions = db.load_sessions();
    let Some(session) = sessions.get("szwwuv") else {
      return;
    };
    assert!(session.workflow.is_none(), "szwwuv persisted before workflow DAG was stored");
    let def_needs =
      crate::daemon::workflow::needs_from_harness_profile_for_test(session).expect("def needs from arith profile");
    assert_eq!(def_needs.get("summarize"), Some(&vec!["add".to_string(), "multiply".to_string()]));
    let meta = crate::daemon::workflow::effective_workflow_meta(session).expect("graph");
    let summarize = meta.nodes.iter().find(|n| n.id == "summarize").expect("summarize");
    assert_eq!(
      summarize.needs,
      vec!["add".to_string(), "multiply".to_string()],
      "profile={:?} kind={:?} repo={}",
      session.profile,
      session.kind,
      session.repo
    );
  }

  fn session(id: &str) -> Session {
    Session {
      id: id.into(),
      started_at: 1,
      ended_at: None,
      profile: Some("default".into()),
      kind: None,
      repo: "/r".into(),
      branch: "main".into(),
      skills: Vec::new(),
      procs: vec![ProcRecord {
        index: 0,
        previous_attempt: None,
        label: "s".into(),
        kind: ProcKind::Skill,
        status: ProcStatus::Ok,
        skill_name: None,
        harness: None,
        model: None,
        started_at: Some(1),
        note: None,
        detail: None,
        fail_reason: None,
        elapsed: Some(1.0),
        lines: Vec::new(),
        container_name: None,
        container_runtime: None,
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
        annotate_target: None,
      }],
      last_seen_at: 1,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
      supervisor: Default::default(),
    }
  }

  /// A fresh temp DB file path (no shared global state, so tests run in parallel).
  fn temp_db_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("scsh-db-test-{}.redb", crate::runtime::random_nonce_6()))
  }

  /// A finished session (clean end, one ✓ proc) started at `t`.
  fn finished_at(id: &str, t: u64) -> Session {
    Session { started_at: t, last_seen_at: t, ended_at: Some(t + 1), ..session(id) }
  }

  #[test]
  fn sync_persists_and_keeps_rows_for_evicted_sessions() {
    let path = temp_db_path();
    let db = StoreDb::open_path(&path).unwrap();
    // Write two sessions.
    let dirty = vec![
      ("aaaaaa".to_string(), jsonio::session_json_store(&session("aaaaaa"))),
      ("bbbbbb".to_string(), jsonio::session_json_store(&session("bbbbbb"))),
    ];
    db.sync(&dirty).unwrap();
    let loaded = db.load_sessions();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded["aaaaaa"].procs[0].elapsed, Some(1.0));

    // A later sync that no longer mentions `b` (evicted from the working set) leaves its
    // row alone: eviction archives, it does not delete.
    db.sync(&[]).unwrap();
    let loaded = db.load_sessions();
    assert_eq!(loaded.len(), 2);
    assert_eq!(db.get("bbbbbb").map(|s| s.branch), Some("main".into()), "the archived row reads back");
    assert!(db.get("zzzzzz").is_none());
    let _ = std::fs::remove_file(&path);
  }

  #[test]
  fn reopen_sees_prior_writes() {
    let path = temp_db_path();
    {
      let db = StoreDb::open_path(&path).unwrap();
      db.sync(&[("persb".to_string(), jsonio::session_json_store(&session("persb")))]).unwrap();
    } // db dropped → redb releases the file lock
    let reopened = StoreDb::open_path(&path).unwrap();
    let store = Store { sessions: reopened.load_sessions(), ..Store::new(DaemonMode::Persistent, 7274, 0) };
    assert_eq!(store.sessions.len(), 1);
    assert_eq!(store.sessions["persb"].branch, "main");
    drop(reopened);
    let _ = std::fs::remove_file(&path);
  }

  #[test]
  fn load_working_set_caps_memory_keeps_running_and_prunes_the_archive() {
    let path = temp_db_path();
    let db = StoreDb::open_path(&path).unwrap();
    // Five finished sessions (started 10, 20, …, 50) and one still-running one, oldest of all.
    let mut dirty: Vec<(String, String)> = (1..=5u64)
      .map(|i| {
        let id = format!("done-{i:02}");
        let json = jsonio::session_json_store(&finished_at(&id, i * 10));
        (id, json)
      })
      .collect();
    let live = Session { started_at: 2, last_seen_at: 2, ended_at: None, ..session("live-1") };
    dirty.push(("live-1".to_string(), jsonio::session_json_store(&live)));
    db.sync(&dirty).unwrap();

    // mem_cap 2, disk_cap 4, at a `now` close enough that live-1 still reads as running:
    // the working set is the running session plus the two NEWEST finished ones…
    let now = 60;
    let working = db.load_working_set(2, 4, now);
    let mut got: Vec<&str> = working.keys().map(String::as_str).collect();
    got.sort_unstable();
    assert_eq!(got, vec!["done-04", "done-05", "live-1"]);
    // …and the archive dropped exactly the oldest finished row beyond the disk cap: the
    // running session is exempt from both caps.
    let remaining = db.load_sessions();
    assert!(!remaining.contains_key("done-01"), "oldest finished row beyond disk_cap is pruned");
    for kept in ["done-02", "done-03", "done-04", "done-05", "live-1"] {
      assert!(remaining.contains_key(kept), "{kept} survives");
    }
    let _ = std::fs::remove_file(&path);
  }
}
