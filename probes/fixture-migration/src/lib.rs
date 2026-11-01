//! Disposable v1-to-v2 schema migration experiment for fixture lifecycle data.
//!
//! This crate has no path-taking public API and performs no filesystem removal.
//! Its tests create synthetic sessions and migrate only their own databases.

#[cfg(test)]
mod probe {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
    };

    use disk_steward_core::{ActionId, AttemptId, ReceiptId, SessionId};
    use disk_steward_fixture_lifecycle::{FixtureLifecycle, FixtureStatus, SessionSnapshot};
    use rusqlite::{Connection, TransactionBehavior};
    use sha2::{Digest, Sha256};

    const APP_ID: i64 = 0x4453_464c;
    const V1: i64 = 1;
    const V2: i64 = 2;
    const MANIFEST_V1: &str = "CREATE TABLE manifest(singleton INTEGER PRIMARY KEY CHECK(singleton=1), session_id TEXT NOT NULL, parent_dev INTEGER NOT NULL,parent_ino INTEGER NOT NULL,root_dev INTEGER NOT NULL,root_ino INTEGER NOT NULL,source_dev INTEGER NOT NULL,source_ino INTEGER NOT NULL,holding_dev INTEGER NOT NULL,holding_ino INTEGER NOT NULL) STRICT";
    const FIXTURES_V1: &str = "CREATE TABLE fixtures(action_id TEXT PRIMARY KEY,slot TEXT NOT NULL UNIQUE,bytes INTEGER NOT NULL,sha256 TEXT NOT NULL,dev INTEGER NOT NULL,ino INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN ('available','staged','restored','uncertain')),receipt_id TEXT UNIQUE) STRICT";
    const ATTEMPTS_V1: &str = "CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY,action_id TEXT NOT NULL,operation TEXT NOT NULL CHECK(operation IN ('stage','restore')),receipt_id TEXT,outcome TEXT NOT NULL CHECK(outcome IN ('intent','staged','restored','collision','uncertain'))) STRICT";
    // Proposed experimental vocabulary only. This is not the runtime contract.
    const FIXTURES_V2: &str = "CREATE TABLE fixtures(action_id TEXT PRIMARY KEY,slot TEXT NOT NULL UNIQUE,bytes INTEGER NOT NULL,sha256 TEXT NOT NULL,dev INTEGER NOT NULL,ino INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN ('available','staged','restored','uncertain','permanently_removed')),receipt_id TEXT UNIQUE) STRICT";
    const ATTEMPTS_V2: &str = "CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY,action_id TEXT NOT NULL,operation TEXT NOT NULL CHECK(operation IN ('stage','restore','purge')),receipt_id TEXT,outcome TEXT NOT NULL CHECK(outcome IN ('intent','staged','restored','collision','uncertain','permanently_removed'))) STRICT";
    const SLOTS: [(&str, i64); 5] = [
        ("compiler-output", 3_145_728),
        ("test-cache", 1_048_576),
        ("preview-cache", 524_288),
        ("package-index", 262_144),
        ("incremental-output", 131_072),
    ];
    const MAX_ROWS: usize = 256;
    const MAX_TEXT: usize = 256;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FixtureRow(
        String,
        String,
        i64,
        String,
        i64,
        i64,
        String,
        Option<String>,
        i64,
    );
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct AttemptRow(String, String, String, Option<String>, String, i64);
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ManifestRow(i64, String, i64, i64, i64, i64, i64, i64, i64, i64);
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Rows {
        manifest: Vec<ManifestRow>,
        fixtures: Vec<FixtureRow>,
        attempts: Vec<AttemptRow>,
    }
    type SchemaObjects = Vec<(String, String, String)>;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Fault {
        None,
        AfterRename,
        AfterCreate,
        AfterCopy,
        BeforeVersionCommit,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum ProbeError {
        Unsupported,
        InvalidRows,
        Injected(Fault),
        Sql,
    }

    fn db_path(parent: &Path, id: &SessionId) -> PathBuf {
        let wire = serde_json::to_value(id)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        let component = wire.rsplit(':').next().unwrap().trim_end_matches('"');
        parent
            .join(format!(".disk-steward-fixtures-{component}"))
            .join("lifecycle.sqlite3")
    }

    // Construct a frozen legacy-format database from a self-created current
    // fixture session. This is test setup only, never an application downgrade.
    fn make_legacy_fixture(path: &Path) {
        let mut conn = Connection::open(path).unwrap();
        let (app, version, objects) = schema(&conn).unwrap();
        assert_eq!(app, APP_ID);
        if version == V1 {
            return;
        }
        assert_eq!(version, V2);
        assert_eq!(
            objects,
            vec![
                ("table".into(), "attempts".into(), ATTEMPTS_V2.into()),
                ("table".into(), "fixtures".into(), FIXTURES_V2.into()),
                ("table".into(), "manifest".into(), MANIFEST_V1.into()),
            ]
        );
        let before = load_rows(&conn).unwrap();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute_batch("ALTER TABLE fixtures RENAME TO fixtures_current_backup; ALTER TABLE attempts RENAME TO attempts_current_backup;").unwrap();
        tx.execute_batch(FIXTURES_V1).unwrap();
        tx.execute_batch(ATTEMPTS_V1).unwrap();
        tx.execute_batch("INSERT INTO fixtures(rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id) SELECT rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id FROM fixtures_current_backup ORDER BY rowid; INSERT INTO attempts(rowid,attempt_id,action_id,operation,receipt_id,outcome) SELECT rowid,attempt_id,action_id,operation,receipt_id,outcome FROM attempts_current_backup ORDER BY rowid; DROP TABLE fixtures_current_backup; DROP TABLE attempts_current_backup;").unwrap();
        assert_eq!(load_rows(&tx).unwrap(), before);
        tx.pragma_update(None, "user_version", V1).unwrap();
        tx.commit().unwrap();
    }

    fn make_session(parent: &Path) -> (SessionId, SessionSnapshot, PathBuf) {
        let actions = std::array::from_fn(|_| ActionId::new());
        let id = SessionId::new();
        let (mut life, initial) =
            FixtureLifecycle::create_for_session(parent, id, actions).unwrap();
        let a = initial.fixtures[0].action_id;
        let b = initial.fixtures[1].action_id;
        let c = initial.fixtures[2].action_id;
        let r1 = life.stage(a, AttemptId::new()).unwrap();
        life.restore(r1, AttemptId::new()).unwrap();
        life.stage(a, AttemptId::new()).unwrap();
        life.stage(b, AttemptId::new()).unwrap();
        let r3 = life.stage(c, AttemptId::new()).unwrap();
        life.restore(r3, AttemptId::new()).unwrap();
        let snapshot = life.snapshot().unwrap();
        assert_eq!(snapshot.fixtures.len(), 5);
        let path = db_path(parent, &id);
        drop(life);
        let (reopened, verified) = FixtureLifecycle::reopen(parent, id).unwrap();
        assert_eq!(verified, snapshot);
        drop(reopened);
        make_legacy_fixture(&path);
        (id, snapshot, path)
    }

    fn schema(conn: &Connection) -> Result<(i64, i64, SchemaObjects), ProbeError> {
        let app: i64 = conn
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .map_err(|_| ProbeError::Sql)?;
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|_| ProbeError::Sql)?;
        let schema_bounds: (i64, i64, i64, i64) = conn.query_row(
            "SELECT count(*), coalesce(max(length(CAST(type AS BLOB))),0), coalesce(max(length(CAST(name AS BLOB))),0), coalesce(max(length(CAST(sql AS BLOB))),0) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).map_err(|_| ProbeError::Sql)?;
        if schema_bounds.0 > 16
            || schema_bounds.1 > 64
            || schema_bounds.2 > 128
            || schema_bounds.3 > 2048
        {
            return Err(ProbeError::Unsupported);
        }
        let mut q = conn.prepare("SELECT type,name,sql FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY type,name").map_err(|_| ProbeError::Sql)?;
        let objects = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|_| ProbeError::Sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ProbeError::Sql)?;
        Ok((app, version, objects))
    }

    fn load_rows(conn: &Connection) -> Result<Rows, ProbeError> {
        let bounds: (i64, i64, i64, i64) = conn.query_row(
            "SELECT (SELECT count(*) FROM manifest), (SELECT count(*) FROM fixtures), (SELECT count(*) FROM attempts), (SELECT max(length(CAST(session_id AS BLOB))) FROM manifest)",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).map_err(|_| ProbeError::Sql)?;
        if bounds.0 != 1
            || bounds.1 != 5
            || bounds.2 < 0
            || bounds.2 as usize > MAX_ROWS
            || bounds.3 > MAX_TEXT as i64
        {
            return Err(ProbeError::InvalidRows);
        }
        let oversized: i64 = conn.query_row(
            "SELECT (SELECT count(*) FROM fixtures WHERE length(CAST(action_id AS BLOB))>128 OR length(CAST(slot AS BLOB))>64 OR length(CAST(sha256 AS BLOB))>64 OR length(CAST(status AS BLOB))>32 OR length(CAST(receipt_id AS BLOB))>128) + (SELECT count(*) FROM attempts WHERE length(CAST(attempt_id AS BLOB))>128 OR length(CAST(action_id AS BLOB))>128 OR length(CAST(operation AS BLOB))>32 OR length(CAST(receipt_id AS BLOB))>128 OR length(CAST(outcome AS BLOB))>32)",
            [], |r| r.get(0),
        ).map_err(|_| ProbeError::Sql)?;
        if oversized != 0 {
            return Err(ProbeError::InvalidRows);
        }
        let manifest = conn.prepare("SELECT singleton,session_id,parent_dev,parent_ino,root_dev,root_ino,source_dev,source_ino,holding_dev,holding_ino FROM manifest ORDER BY singleton")
            .map_err(|_| ProbeError::Sql)?.query_map([], |r| Ok(ManifestRow(r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))
            .map_err(|_| ProbeError::Sql)?.collect::<Result<Vec<_>,_>>().map_err(|_| ProbeError::Sql)?;
        let fixtures = conn.prepare("SELECT action_id,slot,bytes,sha256,dev,ino,status,receipt_id,rowid FROM fixtures ORDER BY rowid")
            .map_err(|_| ProbeError::Sql)?.query_map([], |r| Ok(FixtureRow(r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?)))
            .map_err(|_| ProbeError::Sql)?.collect::<Result<Vec<_>,_>>().map_err(|_| ProbeError::Sql)?;
        let attempts = conn.prepare("SELECT attempt_id,action_id,operation,receipt_id,outcome,rowid FROM attempts ORDER BY rowid")
            .map_err(|_| ProbeError::Sql)?.query_map([], |r| Ok(AttemptRow(r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))
            .map_err(|_| ProbeError::Sql)?.collect::<Result<Vec<_>,_>>().map_err(|_| ProbeError::Sql)?;
        Ok(Rows {
            manifest,
            fixtures,
            attempts,
        })
    }

    fn valid_session(value: &str) -> bool {
        serde_json::from_str::<SessionId>(value).is_ok()
    }
    fn valid_action(value: &str) -> bool {
        serde_json::from_str::<ActionId>(value).is_ok()
    }
    fn valid_attempt(value: &str) -> bool {
        serde_json::from_str::<AttemptId>(value).is_ok()
    }
    fn valid_receipt(value: &str) -> bool {
        serde_json::from_str::<ReceiptId>(value).is_ok()
    }

    fn validate_rows(
        rows: &Rows,
        snapshot: Option<&SessionSnapshot>,
        version: i64,
    ) -> Result<(), ProbeError> {
        if rows.manifest.len() != 1 || rows.fixtures.len() != 5 || rows.attempts.len() > MAX_ROWS {
            return Err(ProbeError::InvalidRows);
        }
        let m = &rows.manifest[0];
        if m.0 != 1
            || !valid_session(&m.1)
            || [m.2, m.3, m.4, m.5, m.6, m.7, m.8, m.9]
                .iter()
                .any(|n| *n < 0)
        {
            return Err(ProbeError::InvalidRows);
        }
        let mut actions = BTreeMap::new();
        for f in &rows.fixtures {
            if !valid_action(&f.0)
                || f.1.len() > MAX_TEXT
                || f.1.is_empty()
                || f.2 <= 0
                || f.2 > 6 * 1024 * 1024
                || f.3 != expected_digest(&f.1, f.2 as u64)
                || f.4 <= 0
                || f.5 <= 0
                || !SLOTS.contains(&(f.1.as_str(), f.2))
                || !matches!(
                    f.6.as_str(),
                    "available" | "staged" | "restored" | "uncertain" | "permanently_removed"
                )
                || f.7.as_ref().is_some_and(|v| !valid_receipt(v))
            {
                return Err(ProbeError::InvalidRows);
            }
            if actions.insert(f.0.as_str(), f).is_some() {
                return Err(ProbeError::InvalidRows);
            }
        }
        let mut attempt_ids = BTreeMap::new();
        for a in &rows.attempts {
            if !valid_attempt(&a.0)
                || !valid_action(&a.1)
                || a.2.len() > MAX_TEXT
                || a.4.len() > MAX_TEXT
                || !actions.contains_key(a.1.as_str())
                || !matches!(a.2.as_str(), "stage" | "restore" | "purge")
                || !matches!(
                    a.4.as_str(),
                    "intent"
                        | "staged"
                        | "restored"
                        | "collision"
                        | "uncertain"
                        | "permanently_removed"
                )
                || a.3.as_ref().is_some_and(|v| !valid_receipt(v))
            {
                return Err(ProbeError::InvalidRows);
            }
            if attempt_ids.insert(a.0.as_str(), ()).is_some() {
                return Err(ProbeError::InvalidRows);
            }
        }
        if let Some(s) = snapshot {
            if serde_json::to_string(&s.session_id).unwrap() != m.1 || s.fixtures.len() != 5 {
                return Err(ProbeError::InvalidRows);
            }
            for item in &s.fixtures {
                let id = serde_json::to_string(&item.action_id).unwrap();
                let f = actions.get(id.as_str()).ok_or(ProbeError::InvalidRows)?;
                let status = match item.status {
                    FixtureStatus::Available => "available",
                    FixtureStatus::Staged => "staged",
                    FixtureStatus::Restored => "restored",
                    FixtureStatus::Uncertain => "uncertain",
                    FixtureStatus::Changed => return Err(ProbeError::InvalidRows),
                };
                let receipt = item.receipt_id.map(|v| serde_json::to_string(&v).unwrap());
                let pending = rows
                    .attempts
                    .iter()
                    .rev()
                    .find(|a| a.1 == id && a.4 == "intent")
                    .and_then(|a| a.3.clone());
                // A pending operation changes the public observation, not the
                // pre-operation facts that the migration must preserve.
                let observed_status = if f.6 == "uncertain" || pending.is_some() {
                    "uncertain"
                } else {
                    f.6.as_str()
                };
                let snapshot_receipt = pending.or_else(|| f.7.clone());
                if !SLOTS
                    .iter()
                    .any(|(slot, bytes)| *slot == f.1 && *bytes as u64 == item.bytes)
                    || f.2 as u64 != item.bytes
                    || f.3 != item.sha256
                    || observed_status != status
                    || snapshot_receipt != receipt
                {
                    return Err(ProbeError::InvalidRows);
                }
            }
        }
        if version == V1 && rows.fixtures.iter().any(|f| f.6 == "permanently_removed")
            || version == V1
                && rows
                    .attempts
                    .iter()
                    .any(|a| a.2 == "purge" || a.4 == "permanently_removed")
        {
            return Err(ProbeError::InvalidRows);
        }
        Ok(())
    }

    fn expected_digest(slot: &str, bytes: u64) -> String {
        let index = SLOTS
            .iter()
            .position(|(candidate, _)| *candidate == slot)
            .unwrap();
        let mut hash = Sha256::new();
        let buffer = [(index as u8).wrapping_add(0x41); 64 * 1024];
        let mut remaining = bytes;
        while remaining > 0 {
            let amount = remaining.min(buffer.len() as u64) as usize;
            hash.update(&buffer[..amount]);
            remaining -= amount as u64;
        }
        format!("{:x}", hash.finalize())
    }

    fn migrate(
        path: &Path,
        snapshot: Option<&SessionSnapshot>,
        fault: Fault,
    ) -> Result<(), ProbeError> {
        let mut conn = Connection::open(path).map_err(|_| ProbeError::Sql)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| ProbeError::Sql)?;
        let (app, version, objects) = schema(&tx)?;
        if app != APP_ID {
            return Err(ProbeError::Unsupported);
        }
        if version == V2 {
            if objects
                != vec![
                    ("table".into(), "attempts".into(), ATTEMPTS_V2.into()),
                    ("table".into(), "fixtures".into(), FIXTURES_V2.into()),
                    ("table".into(), "manifest".into(), MANIFEST_V1.into()),
                ]
            {
                return Err(ProbeError::Unsupported);
            }
            validate_rows(&load_rows(&tx)?, snapshot, V2)?;
            tx.commit().map_err(|_| ProbeError::Sql)?;
            return Ok(());
        }
        if version != V1
            || objects
                != vec![
                    ("table".into(), "attempts".into(), ATTEMPTS_V1.into()),
                    ("table".into(), "fixtures".into(), FIXTURES_V1.into()),
                    ("table".into(), "manifest".into(), MANIFEST_V1.into()),
                ]
        {
            return Err(ProbeError::Unsupported);
        }
        let before = load_rows(&tx)?;
        validate_rows(&before, snapshot, V1)?;
        tx.execute_batch("ALTER TABLE fixtures RENAME TO fixtures_v1_backup; ALTER TABLE attempts RENAME TO attempts_v1_backup;").map_err(|_| ProbeError::Sql)?;
        if fault == Fault::AfterRename {
            return Err(ProbeError::Injected(fault));
        }
        tx.execute_batch(FIXTURES_V2).map_err(|_| ProbeError::Sql)?;
        tx.execute_batch(ATTEMPTS_V2).map_err(|_| ProbeError::Sql)?;
        if fault == Fault::AfterCreate {
            return Err(ProbeError::Injected(fault));
        }
        tx.execute_batch("INSERT INTO fixtures(rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id) SELECT rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id FROM fixtures_v1_backup ORDER BY rowid; INSERT INTO attempts(rowid,attempt_id,action_id,operation,receipt_id,outcome) SELECT rowid,attempt_id,action_id,operation,receipt_id,outcome FROM attempts_v1_backup ORDER BY rowid;").map_err(|_| ProbeError::Sql)?;
        if fault == Fault::AfterCopy {
            return Err(ProbeError::Injected(fault));
        }
        tx.execute_batch("DROP TABLE fixtures_v1_backup; DROP TABLE attempts_v1_backup;")
            .map_err(|_| ProbeError::Sql)?;
        let after = load_rows(&tx)?;
        if before != after {
            return Err(ProbeError::InvalidRows);
        }
        if fault == Fault::BeforeVersionCommit {
            return Err(ProbeError::Injected(fault));
        }
        tx.pragma_update(None, "user_version", V2)
            .map_err(|_| ProbeError::Sql)?;
        tx.commit().map_err(|_| ProbeError::Sql)?;
        Ok(())
    }

    fn ensure_valid_v2(path: &Path, snapshot: &SessionSnapshot) -> Result<Rows, ProbeError> {
        let conn = Connection::open(path).map_err(|_| ProbeError::Sql)?;
        let (app, version, objects) = schema(&conn)?;
        if app != APP_ID
            || version != V2
            || objects
                != vec![
                    ("table".into(), "attempts".into(), ATTEMPTS_V2.into()),
                    ("table".into(), "fixtures".into(), FIXTURES_V2.into()),
                    ("table".into(), "manifest".into(), MANIFEST_V1.into()),
                ]
        {
            return Err(ProbeError::Unsupported);
        }
        let rows = load_rows(&conn)?;
        validate_rows(&rows, Some(snapshot), V2)?;
        Ok(rows)
    }

    #[test]
    fn migrates_real_fixture_history_preserves_rows_and_reruns_idempotently() {
        let parent = tempfile::tempdir().unwrap();
        let (id, snapshot, path) = make_session(parent.path());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "UPDATE fixtures SET rowid=rowid+10000; UPDATE attempts SET rowid=rowid+10000;",
        )
        .unwrap();
        drop(conn);
        let (life, gap_snapshot) = FixtureLifecycle::reopen(parent.path(), id).unwrap();
        assert_eq!(gap_snapshot, snapshot);
        drop(life);
        make_legacy_fixture(&path);
        let conn = Connection::open(&path).unwrap();
        let before = load_rows(&conn).unwrap();
        validate_rows(&before, Some(&snapshot), V1).unwrap();
        drop(conn);
        migrate(&path, Some(&snapshot), Fault::None).unwrap();
        let after = ensure_valid_v2(&path, &snapshot).unwrap();
        assert_eq!(before, after);
        migrate(&path, Some(&snapshot), Fault::None).unwrap();
        assert_eq!(ensure_valid_v2(&path, &snapshot).unwrap(), before);
    }

    #[test]
    fn migration_accepts_valid_session_with_no_attempt_history() {
        let parent = tempfile::tempdir().unwrap();
        let id = SessionId::new();
        let actions = std::array::from_fn(|_| ActionId::new());
        let (life, snapshot) =
            FixtureLifecycle::create_for_session(parent.path(), id, actions).unwrap();
        let path = db_path(parent.path(), &id);
        drop(life);
        make_legacy_fixture(&path);
        assert!(load_rows(&Connection::open(&path).unwrap())
            .unwrap()
            .attempts
            .is_empty());
        migrate(&path, Some(&snapshot), Fault::None).unwrap();
        assert!(ensure_valid_v2(&path, &snapshot)
            .unwrap()
            .attempts
            .is_empty());
    }

    #[test]
    fn persisted_uncertain_state_and_attempt_survive_migration_without_retry() {
        for outcome in ["uncertain", "intent"] {
            let parent = tempfile::tempdir().unwrap();
            let actions = std::array::from_fn(|_| ActionId::new());
            let id = SessionId::new();
            let (life, _) =
                FixtureLifecycle::create_for_session(parent.path(), id, actions).unwrap();
            let path = db_path(parent.path(), &id);
            drop(life);
            let conn = Connection::open(&path).unwrap();
            let action: String = conn
                .query_row(
                    "SELECT action_id FROM fixtures WHERE slot='compiler-output'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let receipt = serde_json::to_string(&ReceiptId::new()).unwrap();
            conn.execute(
                "UPDATE fixtures SET status=?2 WHERE action_id=?1",
                rusqlite::params![
                    &action,
                    if outcome == "intent" {
                        "available"
                    } else {
                        "uncertain"
                    }
                ],
            )
            .unwrap();
            conn.execute("INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'stage',?3,?4)", (serde_json::to_string(&AttemptId::new()).unwrap(), &action, &receipt, outcome)).unwrap();
            drop(conn);
            let (life, snapshot) = FixtureLifecycle::reopen(parent.path(), id).unwrap();
            assert!(snapshot
                .fixtures
                .iter()
                .any(|f| f.status == FixtureStatus::Uncertain
                    && serde_json::to_string(&f.action_id).unwrap() == action
                    && f.receipt_id
                        == if outcome == "intent" {
                            Some(serde_json::from_str::<ReceiptId>(&receipt).unwrap())
                        } else {
                            None
                        }));
            drop(life);
            make_legacy_fixture(&path);
            let before = load_rows(&Connection::open(&path).unwrap()).unwrap();
            migrate(&path, Some(&snapshot), Fault::None).unwrap();
            assert_eq!(ensure_valid_v2(&path, &snapshot).unwrap(), before);
        }
    }

    #[test]
    fn every_injected_failure_rolls_back_schema_and_all_rows() {
        for fault in [
            Fault::AfterRename,
            Fault::AfterCreate,
            Fault::AfterCopy,
            Fault::BeforeVersionCommit,
        ] {
            let parent = tempfile::tempdir().unwrap();
            let (id, snapshot, path) = make_session(parent.path());
            let conn = Connection::open(&path).unwrap();
            let before = load_rows(&conn).unwrap();
            drop(conn);
            assert_eq!(
                migrate(&path, Some(&snapshot), fault),
                Err(ProbeError::Injected(fault))
            );
            let conn = Connection::open(&path).unwrap();
            let (app, version, objects) = schema(&conn).unwrap();
            assert_eq!((app, version), (APP_ID, V1));
            assert_eq!(
                objects,
                vec![
                    ("table".into(), "attempts".into(), ATTEMPTS_V1.into()),
                    ("table".into(), "fixtures".into(), FIXTURES_V1.into()),
                    ("table".into(), "manifest".into(), MANIFEST_V1.into())
                ]
            );
            assert_eq!(load_rows(&conn).unwrap(), before);
            drop(conn);
            let (life, reopened) = FixtureLifecycle::reopen(parent.path(), id).unwrap();
            assert_eq!(reopened, snapshot);
            drop(life);
        }
    }

    #[test]
    fn extra_schema_future_version_and_malformed_rows_are_preserved_and_refused() {
        for ddl in [
            "CREATE VIEW unexpected AS SELECT 1 AS value;",
            "CREATE TRIGGER unexpected_trigger AFTER UPDATE ON fixtures BEGIN SELECT 1; END;",
            "CREATE TABLE sqliteXevil(value TEXT);",
        ] {
            let parent = tempfile::tempdir().unwrap();
            let (_id, snapshot, path) = make_session(parent.path());
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(ddl).unwrap();
            let before = load_rows(&conn).unwrap();
            let before_schema = schema(&conn).unwrap();
            drop(conn);
            assert_eq!(
                migrate(&path, Some(&snapshot), Fault::None),
                Err(ProbeError::Unsupported)
            );
            let conn = Connection::open(&path).unwrap();
            assert_eq!(load_rows(&conn).unwrap(), before);
            assert_eq!(schema(&conn).unwrap(), before_schema);
        }

        let parent = tempfile::tempdir().unwrap();
        let (_id, snapshot, path) = make_session(parent.path());
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 9).unwrap();
        let before = load_rows(&conn).unwrap();
        drop(conn);
        assert_eq!(
            migrate(&path, Some(&snapshot), Fault::None),
            Err(ProbeError::Unsupported)
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            9
        );
        assert_eq!(load_rows(&conn).unwrap(), before);
        drop(conn);

        let parent = tempfile::tempdir().unwrap();
        let (_id, _snapshot, path) = make_session(parent.path());
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE fixtures SET sha256=?1 WHERE rowid=(SELECT rowid FROM fixtures LIMIT 1)",
            ["bad".to_string()],
        )
        .unwrap();
        let before = load_rows(&conn).unwrap();
        drop(conn);
        assert_eq!(
            migrate(&path, None, Fault::None),
            Err(ProbeError::InvalidRows)
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(load_rows(&conn).unwrap(), before);
        drop(conn);

        let parent = tempfile::tempdir().unwrap();
        let (_id, _snapshot, path) = make_session(parent.path());
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE attempts SET attempt_id=action_id WHERE rowid=(SELECT rowid FROM attempts LIMIT 1)", []).unwrap();
        let before = load_rows(&conn).unwrap();
        drop(conn);
        assert_eq!(
            migrate(&path, None, Fault::None),
            Err(ProbeError::InvalidRows)
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(load_rows(&conn).unwrap(), before);
        drop(conn);
    }
}
