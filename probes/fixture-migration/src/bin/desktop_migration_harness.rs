//! External helper for a migration exercise using a disposable desktop HOME.
//! The root is fixed below from a disposable HOME; no arbitrary path input.
use std::{env, fs, path::PathBuf};

use disk_steward_demo_execution::DemoExecutionService;
use disk_steward_review_plan::{Decision, ReviewPlanStore};
use rusqlite::{types::Value, Connection};
use sha2::{Digest, Sha256};

const FIXTURES_V1: &str = "CREATE TABLE fixtures(action_id TEXT PRIMARY KEY,slot TEXT NOT NULL UNIQUE,bytes INTEGER NOT NULL,sha256 TEXT NOT NULL,dev INTEGER NOT NULL,ino INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN ('available','staged','restored','uncertain')),receipt_id TEXT UNIQUE) STRICT";
const ATTEMPTS_V1: &str = "CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY,action_id TEXT NOT NULL,operation TEXT NOT NULL CHECK(operation IN ('stage','restore')),receipt_id TEXT,outcome TEXT NOT NULL CHECK(outcome IN ('intent','staged','restored','collision','uncertain'))) STRICT";
const LIFECYCLE_APP_ID: i64 = 0x4453_464c;

fn app_root() -> PathBuf {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .expect("isolated HOME required");
    let canonical = fs::canonicalize(&home).expect("HOME must exist");
    let temp = fs::canonicalize(env::temp_dir()).expect("temp directory");
    assert!(
        canonical.starts_with(&temp),
        "refusing HOME outside the system temp directory"
    );
    assert!(
        !fs::symlink_metadata(&home)
            .unwrap()
            .file_type()
            .is_symlink(),
        "HOME must not be a symlink"
    );
    assert_eq!(
        fs::read(canonical.join(".disk-steward-disposable-probe"))
            .expect("disposable probe marker"),
        b"disk-steward-migration-v1\n"
    );
    let root = canonical.join("Library/Application Support/com.disksteward.desktop");
    let mut component = canonical.clone();
    for part in ["Library", "Application Support", "com.disksteward.desktop"] {
        component.push(part);
        if let Ok(metadata) = fs::symlink_metadata(&component) {
            assert!(
                !metadata.file_type().is_symlink(),
                "app-data path components must not be symlinks"
            );
        }
    }
    if root.exists() {
        assert!(
            fs::canonicalize(&root).unwrap().starts_with(&canonical),
            "app-data root escaped disposable HOME"
        );
    }
    root
}

fn tables(db: &Connection) -> Vec<String> {
    let mut q = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    q.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn rows(db: &Connection) -> serde_json::Value {
    let mut result = serde_json::Map::new();
    for table in tables(db) {
        let columns: Vec<String> = db
            .prepare(&format!(
                "SELECT name FROM pragma_table_info('{table}') ORDER BY cid"
            ))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let select = format!(
            "SELECT rowid, {} FROM {table} ORDER BY rowid",
            columns.join(",")
        );
        let mut stmt = db.prepare(&select).unwrap();
        let mut dump = Vec::new();
        let mut iter = stmt.query([]).unwrap();
        while let Some(row) = iter.next().unwrap() {
            let mut vals = vec![serde_json::json!(row.get::<_, i64>(0).unwrap())];
            for ix in 0..columns.len() {
                let v: Value = row.get(ix + 1).unwrap();
                vals.push(match v {
                    Value::Null => serde_json::Value::Null,
                    Value::Integer(x) => serde_json::json!(x),
                    Value::Real(x) => serde_json::json!(x),
                    Value::Text(x) => serde_json::json!(x),
                    Value::Blob(x) => serde_json::json!(x),
                });
            }
            dump.push(serde_json::Value::Array(vals));
        }
        result.insert(table, serde_json::Value::Array(dump));
    }
    serde_json::Value::Object(result)
}

fn lifecycle_db(root: &std::path::Path) -> PathBuf {
    fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".disk-steward-fixtures-")
                && p.join("lifecycle.sqlite3").is_file()
        })
        .expect("fixture lifecycle directory")
        .join("lifecycle.sqlite3")
}

fn file_digests(root: &std::path::Path) -> serde_json::Value {
    let mut result = serde_json::Map::new();
    let session = fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".disk-steward-fixtures-")
        })
        .unwrap();
    for dir in [session.join("source"), session.join("holding")] {
        for entry in fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_file() {
                let bytes = fs::read(&path).unwrap();
                result.insert(path.strip_prefix(&session).unwrap().to_string_lossy().into_owned(), serde_json::json!({"bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(bytes))}));
            }
        }
    }
    serde_json::Value::Object(result)
}

fn seed() {
    let root = app_root();
    assert!(
        !root.exists(),
        "refusing to seed an existing app-data directory"
    );
    fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut service = DemoExecutionService::open(&root).expect("service open");
    service.start_demo().expect("start demo");
    let mut plan_store = ReviewPlanStore::open(root.join("review-example-v1.sqlite3")).unwrap();
    let mut plan = plan_store.get_plan().unwrap();
    plan = plan_store
        .edit(plan.revision_id, plan.actions[0].id, Decision::Include)
        .unwrap();
    let stage = service.prepare_stage(plan.revision_id).unwrap();
    let staged = service
        .confirm_and_execute(stage.review_id, |_| true)
        .unwrap();
    let receipt = staged.items[0].receipt_id.unwrap();
    let restore = service.prepare_restore(plan.revision_id, receipt).unwrap();
    service
        .confirm_and_execute(restore.review_id, |_| true)
        .unwrap();
    let stage_again = service.prepare_stage(plan.revision_id).unwrap();
    let staged_again = service
        .confirm_and_execute(stage_again.review_id, |_| true)
        .unwrap();
    let receipt = staged_again.items[0].receipt_id.unwrap();
    drop(service);
    let db_path = lifecycle_db(&root);
    let db = Connection::open(&db_path).unwrap();
    let app_id: i64 = db
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .unwrap();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(app_id, LIFECYCLE_APP_ID);
    assert_eq!(
        version, 2,
        "only the current lifecycle schema can be downgraded by this fixture helper"
    );
    assert_eq!(tables(&db), vec!["attempts", "fixtures", "manifest"]);
    let v2_before = rows(&db);
    let bytes_before = file_digests(&root);
    let tx = db.unchecked_transaction().unwrap();
    tx.execute_batch("ALTER TABLE fixtures RENAME TO fixtures_v2_backup; ALTER TABLE attempts RENAME TO attempts_v2_backup;").unwrap();
    tx.execute_batch(&format!("{FIXTURES_V1};{ATTEMPTS_V1};"))
        .unwrap();
    tx.execute_batch("INSERT INTO fixtures(rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id) SELECT rowid,action_id,slot,bytes,sha256,dev,ino,status,receipt_id FROM fixtures_v2_backup ORDER BY rowid; INSERT INTO attempts(rowid,attempt_id,action_id,operation,receipt_id,outcome) SELECT rowid,attempt_id,action_id,operation,receipt_id,outcome FROM attempts_v2_backup ORDER BY rowid; DROP TABLE fixtures_v2_backup; DROP TABLE attempts_v2_backup;").unwrap();
    tx.pragma_update(None, "user_version", 1).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        rows(&db),
        v2_before,
        "v1 downgrade preserves every rowid and column"
    );
    fs::write(
        root.join("fixture-migration-expected.json"),
        serde_json::to_vec(&v2_before).unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("fixture-migration-bytes.json"),
        serde_json::to_vec(&bytes_before).unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("fixture-migration-receipt.json"),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    fs::write(
        root.join("fixture-migration-db-path.txt"),
        db_path.to_string_lossy().as_bytes(),
    )
    .unwrap();
    println!("Seeded {} with five fixture rows and stage/restore/stage history; lifecycle user_version=1", root.display());
}

fn verify() {
    let root = app_root();
    let db_path = lifecycle_db(&root);
    let db = Connection::open(&db_path).unwrap();
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        version, 2,
        "the desktop startup/status path must have migrated before verification"
    );
    let expected: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("fixture-migration-expected.json")).unwrap())
            .unwrap();
    assert_eq!(
        rows(&db),
        expected,
        "migration must preserve all rowids and columns"
    );
    drop(db);
    let bytes_expected: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("fixture-migration-bytes.json")).unwrap())
            .unwrap();
    assert_eq!(
        file_digests(&root),
        bytes_expected,
        "migration must leave all source and holding bytes unchanged"
    );
    let mut service = DemoExecutionService::open(&root).unwrap();
    let status = service.status().unwrap();
    assert_eq!(
        status
            .fixtures
            .iter()
            .filter(|f| f.status == disk_steward_fixture_lifecycle::FixtureStatus::Staged)
            .count(),
        1
    );
    let receipt: disk_steward_core::ReceiptId =
        serde_json::from_slice(&fs::read(root.join("fixture-migration-receipt.json")).unwrap())
            .unwrap();
    assert!(status.fixtures.iter().any(|f| f.status
        == disk_steward_fixture_lifecycle::FixtureStatus::Staged
        && f.receipt_id == Some(receipt)));
    let plan = ReviewPlanStore::open(root.join("review-example-v1.sqlite3"))
        .unwrap()
        .get_plan()
        .unwrap();
    let restore = service
        .prepare_restore(plan.revision_id, receipt)
        .expect("preserved staged receipt should prepare a restore");
    assert_eq!(restore.items.len(), 1);
    println!("Verified v1→v2, all lifecycle rows/rowids and fixture bytes unchanged, preserved staged receipt prepares restore.");
}

fn service_status() {
    let root = app_root();
    let mut service = DemoExecutionService::open(&root).expect("open disposable service");
    let status = service
        .status()
        .expect("public status path reopens the session");
    println!(
        "Public DemoExecutionService::status reopened session {:?} with {} fixtures.",
        status.session_id.unwrap(),
        status.fixtures.len()
    );
}

fn main() {
    match env::args().nth(1).as_deref() {
        Some("seed") => seed(),
        Some("status") => service_status(),
        Some("verify") => verify(),
        _ => panic!("usage: desktop_migration_harness seed|status|verify"),
    }
}
