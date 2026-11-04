//! Bounded, private persistence for subtree scan replay metadata.
//! This store never retains or opens a selected source path.

use super::checkpoint_store::{
    check_resources, check_resources_with_free_bytes, check_sidecars, ensure_private_file,
    private_database_path, MAX_DATABASE_BYTES, RESERVED_BYTES,
};
use super::subtree_replay::{validate_payload, ReplayInfo, ReplayPayload};
use disk_steward_core::InventoryItemId;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use std::path::{Path, PathBuf};

const APP_ID: i64 = 0x4453_5354; // DSST
const SCHEMA_VERSION: i64 = 1;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
const SCHEMA: &str = "CREATE TABLE subtree_checkpoint(singleton INTEGER PRIMARY KEY CHECK(singleton=1),payload TEXT NOT NULL) STRICT";

pub(super) struct Store {
    connection: Connection,
    path: PathBuf,
}

impl Store {
    pub(super) fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = private_database_path(path.as_ref())?;
        check_resources(&path)?;
        check_sidecars(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| unavailable())?;
        connection
            .busy_timeout(std::time::Duration::from_millis(500))
            .map_err(|_| unavailable())?;
        connection
            .pragma_update(None, "trusted_schema", false)
            .map_err(|_| unavailable())?;
        let page_size: i64 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(|_| corrupt())?;
        if page_size <= 0 {
            return Err(corrupt());
        }
        connection
            .pragma_update(
                None,
                "max_page_count",
                MAX_DATABASE_BYTES as i64 / page_size,
            )
            .map_err(|_| unavailable())?;
        initialize(&connection)?;
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .map_err(|_| unavailable())?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|_| unavailable())?;
        ensure_private_file(&path)?;
        let store = Self { connection, path };
        store.get()?;
        Ok(store)
    }

    pub(super) fn get(&self) -> Result<Option<ReplayPayload>, String> {
        self.check_bounds()?;
        load_payload(&self.connection)
    }

    pub(super) fn save(
        &mut self,
        expected: Option<InventoryItemId>,
        payload: ReplayPayload,
    ) -> Result<ReplayInfo, String> {
        validate_payload(&payload).map_err(|_| "checkpoint_invalid".to_owned())?;
        let text = serde_json::to_string(&payload).map_err(|_| "checkpoint_invalid".to_owned())?;
        if text.len() > MAX_PAYLOAD_BYTES {
            return Err("checkpoint_limit".to_owned());
        }
        self.check_writable_bounds()?;
        let current_size = std::fs::metadata(&self.path)
            .map_err(|_| unavailable())?
            .len();
        if current_size
            .saturating_add(text.len() as u64)
            .saturating_add(4096)
            > MAX_DATABASE_BYTES
        {
            return Err("checkpoint_store_over_budget".to_owned());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unavailable())?;
        let current = load_payload(&tx)?;
        if current.as_ref().map(|value| value.checkpoint_id) != expected {
            return Err("stale_checkpoint".to_owned());
        }
        tx.execute(
            "INSERT INTO subtree_checkpoint(singleton,payload) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET payload=excluded.payload",
            [text],
        )
        .map_err(|_| unavailable())?;
        let pages: i64 = tx
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(|_| corrupt())?;
        let page_size: i64 = tx
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(|_| corrupt())?;
        if pages
            .checked_mul(page_size)
            .is_none_or(|bytes| bytes < 0 || bytes as u64 > MAX_DATABASE_BYTES)
        {
            return Err("checkpoint_store_over_budget".to_owned());
        }
        tx.commit().map_err(|_| unavailable())?;
        Ok(ReplayInfo::from(&payload))
    }

    pub(super) fn clear(&mut self, expected: InventoryItemId) -> Result<(), String> {
        self.check_writable_bounds()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| unavailable())?;
        let current = load_payload(&tx)?.ok_or_else(|| "stale_checkpoint".to_owned())?;
        if current.checkpoint_id != expected {
            return Err("stale_checkpoint".to_owned());
        }
        tx.execute("DELETE FROM subtree_checkpoint WHERE singleton=1", [])
            .map_err(|_| unavailable())?;
        tx.commit().map_err(|_| unavailable())
    }

    fn check_bounds(&self) -> Result<(), String> {
        ensure_private_file(&self.path)?;
        check_sidecars(&self.path)?;
        let size = std::fs::metadata(&self.path)
            .map_err(|_| unavailable())?
            .len();
        if size > MAX_DATABASE_BYTES {
            return Err("checkpoint_store_over_budget".to_owned());
        }
        check_resources(&self.path)
    }

    fn check_writable_bounds(&self) -> Result<(), String> {
        self.check_bounds()?;
        check_resources_with_free_bytes(&self.path, RESERVED_BYTES)
    }
}

fn load_payload(connection: &Connection) -> Result<Option<ReplayPayload>, String> {
    let length: Option<i64> = connection
        .query_row(
            "SELECT length(CAST(payload AS BLOB)) FROM subtree_checkpoint WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| corrupt())?;
    let Some(length) = length else {
        return Ok(None);
    };
    if length < 0 || length as usize > MAX_PAYLOAD_BYTES {
        return Err(corrupt());
    }
    let text: String = connection
        .query_row(
            "SELECT payload FROM subtree_checkpoint WHERE singleton=1 AND length(CAST(payload AS BLOB))<=?1",
            [MAX_PAYLOAD_BYTES as i64],
            |row| row.get(0),
        )
        .map_err(|_| corrupt())?;
    if text.len() > MAX_PAYLOAD_BYTES {
        return Err(corrupt());
    }
    let payload: ReplayPayload = serde_json::from_str(&text).map_err(|_| corrupt())?;
    validate_payload(&payload).map_err(|_| corrupt())?;
    Ok(Some(payload))
}

fn initialize(connection: &Connection) -> Result<(), String> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| unavailable())?;
    let mut app_id: i64 = tx
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(|_| corrupt())?;
    let mut version: i64 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| corrupt())?;
    let mut count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| corrupt())?;
    if app_id == 0 && version == 0 && count == 0 {
        tx.execute_batch(SCHEMA).map_err(|_| unavailable())?;
        tx.pragma_update(None, "application_id", APP_ID)
            .map_err(|_| unavailable())?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|_| unavailable())?;
        app_id = APP_ID;
        version = SCHEMA_VERSION;
        count = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| corrupt())?;
    }
    if app_id != APP_ID || version != SCHEMA_VERSION || count != 1 {
        return Err("checkpoint_store_unsupported".to_owned());
    }
    let schema: Option<String> = tx
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='subtree_checkpoint'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| corrupt())?;
    if schema.as_deref().map(normalize_sql).as_deref() != Some(normalize_sql(SCHEMA).as_str()) {
        return Err("checkpoint_store_unsupported".to_owned());
    }
    tx.commit().map_err(|_| unavailable())
}

fn normalize_sql(sql: &str) -> String {
    sql.chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

fn unavailable() -> String {
    "checkpoint_store_unavailable".to_owned()
}
fn corrupt() -> String {
    "checkpoint_store_corrupt".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder_inventory::checkpoint_store::RootStamp;
    use std::os::unix::fs::PermissionsExt;

    fn private_path() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("subtree-v1.sqlite3");
        (temp, path)
    }

    fn payload() -> ReplayPayload {
        ReplayPayload {
            version: 1,
            checkpoint_id: InventoryItemId::new(),
            root_label: "selected folder".to_owned(),
            root_stamp: RootStamp {
                device: 1,
                inode: 2,
                mode: 0o040755,
                size: 0,
                nlink: 2,
                mtime: 0,
                mtime_nsec: 0,
                ctime: 0,
                ctime_nsec: 0,
                birth_seconds: 0,
                birth_nanos: 0,
            },
            exclusion_revision: disk_steward_core::RevisionId::new(),
            batches: 1,
            entries_observed: 1,
            transcript: [7; 32],
        }
    }

    #[test]
    fn persists_and_reopens_with_compare_and_swap() {
        let (_temp, path) = private_path();
        let mut store = Store::open(&path).unwrap();
        let saved = payload();
        let id = saved.checkpoint_id;
        store.save(None, saved.clone()).unwrap();
        assert_eq!(store.get().unwrap(), Some(saved.clone()));
        assert_eq!(store.save(None, payload()).unwrap_err(), "stale_checkpoint");
        drop(store);
        let mut reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.get().unwrap(), Some(saved));
        assert_eq!(
            reopened.clear(InventoryItemId::new()).unwrap_err(),
            "stale_checkpoint"
        );
        reopened.clear(id).unwrap();
        assert_eq!(reopened.get().unwrap(), None);
    }

    #[test]
    fn rejects_oversized_payload_and_unknown_schema() {
        let (_temp, path) = private_path();
        let mut too_large = payload();
        too_large.root_label = "x".repeat(MAX_PAYLOAD_BYTES);
        let mut store = Store::open(&path).unwrap();
        assert!(store.save(None, too_large).is_err());
        drop(store);

        let (_temp, path) = private_path();
        let _store = Store::open(&path).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);
        assert_eq!(
            Store::open(&path).err().unwrap(),
            "checkpoint_store_unsupported"
        );
    }

    #[test]
    fn refuses_non_private_database_file() {
        let (_temp, path) = private_path();
        let _store = Store::open(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            Store::open(&path).err().unwrap(),
            "checkpoint_store_unavailable"
        );
    }

    #[test]
    fn corrupt_and_oversized_records_fail_closed() {
        let (_temp, path) = private_path();
        let _store = Store::open(&path).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO subtree_checkpoint(singleton,payload) VALUES(1,'{}')",
                [],
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            Store::open(&path).err().unwrap(),
            "checkpoint_store_corrupt"
        );

        let (_temp, path) = private_path();
        let _store = Store::open(&path).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO subtree_checkpoint(singleton,payload) VALUES(1,?1)",
                ["x".repeat(MAX_PAYLOAD_BYTES + 1)],
            )
            .unwrap();
        drop(connection);
        assert_eq!(
            Store::open(&path).err().unwrap(),
            "checkpoint_store_corrupt"
        );
    }
}
