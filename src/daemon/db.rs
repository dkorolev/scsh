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

use std::collections::{BTreeMap, HashSet};

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
  /// failing the load, so one bad record can't wedge the daemon on startup.
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

  /// One-transaction sync: write each `(id, json)` in `dirty`, then delete any stored session
  /// whose id is not in `keep` (the full set of live ids) — that removes evicted sessions.
  pub fn sync(&self, dirty: &[(String, String)], keep: &HashSet<String>) -> Result<(), String> {
    let txn = self.db.begin_write().map_err(|e| e.to_string())?;
    {
      let mut table = txn.open_table(SESSIONS).map_err(|e| e.to_string())?;
      for (id, json) in dirty {
        table.insert(id.as_str(), json.as_bytes()).map_err(|e| e.to_string())?;
      }
      let orphans: Vec<String> = {
        let iter = table.iter().map_err(|e| e.to_string())?;
        iter.flatten().map(|(k, _)| k.value().to_string()).filter(|k| !keep.contains(k)).collect()
      };
      for id in orphans {
        table.remove(id.as_str()).map_err(|e| e.to_string())?;
      }
    }
    txn.commit().map_err(|e| e.to_string())?;
    Ok(())
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
        cast_path: None,
        diff_path: None,
        skill_source: None,
        route: None,
        result_path: None,
      }],
      last_seen_at: 1,
      client_connected: false,
      run_pid: None,
      workflow: None,
      parent_session: None,
    }
  }

  /// A fresh temp DB file path (no shared global state, so tests run in parallel).
  fn temp_db_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("scsh-db-test-{}.redb", crate::runtime::random_nonce_6()))
  }

  fn ids(list: &[&str]) -> HashSet<String> {
    list.iter().map(|s| s.to_string()).collect()
  }

  #[test]
  fn sync_persists_deletes_and_reloads() {
    let path = temp_db_path();
    let db = StoreDb::open_path(&path).unwrap();
    // Write two sessions.
    let dirty = vec![
      ("aaaaaa".to_string(), jsonio::session_json_store(&session("aaaaaa"))),
      ("bbbbbb".to_string(), jsonio::session_json_store(&session("bbbbbb"))),
    ];
    db.sync(&dirty, &ids(&["aaaaaa", "bbbbbb"])).unwrap();
    let loaded = db.load_sessions();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded["aaaaaa"].procs[0].elapsed, Some(1.0));

    // Drop `b` from `keep` → it is deleted; `a` survives.
    db.sync(&[], &ids(&["aaaaaa"])).unwrap();
    let loaded = db.load_sessions();
    assert_eq!(loaded.len(), 1);
    assert!(loaded.contains_key("aaaaaa") && !loaded.contains_key("bbbbbb"));
    let _ = std::fs::remove_file(&path);
  }

  #[test]
  fn reopen_sees_prior_writes() {
    let path = temp_db_path();
    {
      let db = StoreDb::open_path(&path).unwrap();
      db.sync(&[("persb".to_string(), jsonio::session_json_store(&session("persb")))], &ids(&["persb"])).unwrap();
    } // db dropped → redb releases the file lock
    let reopened = StoreDb::open_path(&path).unwrap();
    let store = Store { sessions: reopened.load_sessions(), ..Store::new(DaemonMode::Persistent, 7274, 0) };
    assert_eq!(store.sessions.len(), 1);
    assert_eq!(store.sessions["persb"].branch, "main");
    drop(reopened);
    let _ = std::fs::remove_file(&path);
  }
}
