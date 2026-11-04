//! Private, bounded persistence for an explicitly saved folder paging checkpoint.
//! This module stores metadata only and never opens the selected folder.

use disk_steward_core::{InventoryItemId, RevisionId};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const APP_ID: i64 = 0x4453_4643; // DSFC
const SCHEMA_VERSION: i64 = 1;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
pub(super) const MAX_DATABASE_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const RESERVED_BYTES: u64 = 2 * 1024 * 1024 * 1024 + 64 * 1024 * 1024;
const SCHEMA: &str = "CREATE TABLE folder_checkpoint(singleton INTEGER PRIMARY KEY CHECK(singleton=1),payload TEXT NOT NULL) STRICT";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RootStamp {
    pub device: u64,
    pub inode: u64,
    pub mode: u32,
    pub size: i64,
    pub nlink: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
    pub birth_seconds: i64,
    pub birth_nanos: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Payload {
    pub version: u32,
    pub checkpoint_id: InventoryItemId,
    pub root_label: String,
    pub page_number: u32,
    pub entries_seen: u64,
    pub root_stamp: RootStamp,
    pub exclusion_revision: RevisionId,
    pub replay_names: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Info {
    pub checkpoint_id: InventoryItemId,
    pub root_label: String,
    pub page_number: u32,
    pub entries_seen: String,
}

impl From<&Payload> for Info {
    fn from(value: &Payload) -> Self {
        Self {
            checkpoint_id: value.checkpoint_id,
            root_label: value.root_label.clone(),
            page_number: value.page_number,
            entries_seen: value.entries_seen.to_string(),
        }
    }
}

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
        .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        connection
            .busy_timeout(std::time::Duration::from_millis(500))
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        connection
            .pragma_update(None, "trusted_schema", false)
            .map_err(|_| unavailable())?;
        let page_size: i64 = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(|_| unavailable())?;
        if page_size <= 0 {
            return Err(unavailable());
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
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        ensure_private_file(&path)?;
        let store = Self { connection, path };
        store.get()?;
        Ok(store)
    }

    pub(super) fn get(&self) -> Result<Option<Payload>, String> {
        self.check_bounds()?;
        load_payload(&self.connection)
    }

    pub(super) fn save(
        &mut self,
        expected: Option<InventoryItemId>,
        payload: Payload,
    ) -> Result<Info, String> {
        validate_payload(&payload)?;
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
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        let current = load_payload(&tx)?;
        if current.as_ref().map(|row| row.checkpoint_id) != expected {
            return Err("stale_checkpoint".to_owned());
        }
        tx.execute(
            "INSERT INTO folder_checkpoint(singleton,payload) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET payload=excluded.payload",
            [text],
        )
        .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        let pages: i64 = tx
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(|_| unavailable())?;
        let page_size: i64 = tx
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .map_err(|_| unavailable())?;
        if pages
            .checked_mul(page_size)
            .is_none_or(|bytes| bytes < 0 || bytes as u64 > MAX_DATABASE_BYTES)
        {
            return Err("checkpoint_store_over_budget".into());
        }
        tx.commit()
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        Ok(Info::from(&payload))
    }

    pub(super) fn clear(&mut self, expected: InventoryItemId) -> Result<(), String> {
        self.check_writable_bounds()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        let current = load_payload(&tx)?.ok_or_else(|| "stale_checkpoint".to_owned())?;
        if current.checkpoint_id != expected {
            return Err("stale_checkpoint".to_owned());
        }
        tx.execute("DELETE FROM folder_checkpoint WHERE singleton=1", [])
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        tx.commit()
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        Ok(())
    }

    fn check_bounds(&self) -> Result<(), String> {
        ensure_private_file(&self.path)?;
        check_sidecars(&self.path)?;
        let size = std::fs::metadata(&self.path)
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?
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

fn validate_payload(payload: &Payload) -> Result<(), String> {
    if payload.version != 1
        || payload.root_label.is_empty()
        || payload.root_label.len() > 480
        || payload
            .root_label
            .chars()
            .any(|ch| ch.is_control() || is_bidi_control(ch))
        || !(1..=127).contains(&payload.page_number)
        || payload.entries_seen == 0
        || payload.entries_seen > 65_536
        || payload.entries_seen > u64::from(payload.page_number) * 512
        || payload.root_stamp.mode & 0o170000 != 0o040000
        || payload.root_stamp.nlink == 0
        || payload.replay_names.len() != payload.entries_seen as usize
        || payload.replay_names.len() > 65_536
        || payload.root_stamp.size < 0
        || !(0..1_000_000_000).contains(&payload.root_stamp.mtime_nsec)
        || !(0..1_000_000_000).contains(&payload.root_stamp.ctime_nsec)
        || !(0..1_000_000_000).contains(&payload.root_stamp.birth_nanos)
    {
        return Err("checkpoint_invalid".to_owned());
    }
    let mut names = std::collections::HashSet::with_capacity(payload.replay_names.len());
    for name in &payload.replay_names {
        if name.is_empty()
            || name.len() > 255
            || name.contains(&0)
            || name.contains(&b'/')
            || name == b"."
            || name == b".."
            || !names.insert(name)
        {
            return Err("checkpoint_invalid".to_owned());
        }
    }
    let encoded = serde_json::to_vec(payload).map_err(|_| "checkpoint_invalid".to_owned())?;
    if encoded.len() > MAX_PAYLOAD_BYTES {
        return Err("checkpoint_limit".to_owned());
    }
    Ok(())
}

fn load_payload(connection: &Connection) -> Result<Option<Payload>, String> {
    let length: Option<i64> = connection
        .query_row(
            "SELECT length(CAST(payload AS BLOB)) FROM folder_checkpoint WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "checkpoint_store_corrupt".to_owned())?;
    let Some(length) = length else {
        return Ok(None);
    };
    if length < 0 || length as usize > MAX_PAYLOAD_BYTES {
        return Err("checkpoint_store_corrupt".to_owned());
    }
    let text:String = connection.query_row("SELECT payload FROM folder_checkpoint WHERE singleton=1 AND length(CAST(payload AS BLOB))<=?1",[MAX_PAYLOAD_BYTES as i64],|row|row.get(0)).map_err(|_|"checkpoint_store_corrupt".to_owned())?;
    if text.len() > MAX_PAYLOAD_BYTES {
        return Err("checkpoint_store_corrupt".to_owned());
    }
    let payload: Payload =
        serde_json::from_str(&text).map_err(|_| "checkpoint_store_corrupt".to_owned())?;
    validate_payload(&payload)?;
    Ok(Some(payload))
}

fn initialize(connection: &Connection) -> Result<(), String> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
    let mut app_id: i64 = tx
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(|_| "checkpoint_store_corrupt".to_owned())?;
    let mut version: i64 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| "checkpoint_store_corrupt".to_owned())?;
    let mut count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "checkpoint_store_corrupt".to_owned())?;
    if app_id == 0 && version == 0 && count == 0 {
        tx.execute_batch(SCHEMA)
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        tx.pragma_update(None, "application_id", APP_ID)
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|_| "checkpoint_store_unavailable".to_owned())?;
        app_id = APP_ID;
        version = SCHEMA_VERSION;
        count = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| "checkpoint_store_corrupt".to_owned())?;
    }
    if app_id != APP_ID || version != SCHEMA_VERSION || count != 1 {
        return Err("checkpoint_store_unsupported".to_owned());
    }
    let schema: Option<String> = tx
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='folder_checkpoint'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "checkpoint_store_corrupt".to_owned())?;
    if schema.as_deref().map(normalize_sql).as_deref() != Some(normalize_sql(SCHEMA).as_str()) {
        return Err("checkpoint_store_unsupported".to_owned());
    }
    tx.commit()
        .map_err(|_| "checkpoint_store_unavailable".to_owned())
}

fn normalize_sql(sql: &str) -> String {
    sql.chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_bidi_control(ch: char) -> bool {
    matches!(ch as u32, 0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069)
}

pub(super) fn private_database_path(path: &Path) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let name = path.file_name().ok_or_else(unavailable)?;
        let parent = path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let metadata = std::fs::symlink_metadata(parent).map_err(|_| unavailable())?;
        let uid = rustix::process::geteuid().as_raw();
        if !metadata.file_type().is_dir()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(unavailable());
        }
        let parent = std::fs::canonicalize(parent).map_err(|_| unavailable())?;
        let candidate = parent.join(name);
        check_resources_with_free_bytes(&candidate, RESERVED_BYTES)?;
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta)
                if meta.file_type().is_file()
                    && meta.uid() == uid
                    && meta.mode() & 0o777 == 0o600
                    && meta.nlink() == 1 => {}
            Ok(_) => return Err(unavailable()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&candidate)
                    .map_err(|_| unavailable())?;
                drop(file);
            }
            Err(_) => return Err(unavailable()),
        }
        Ok(candidate)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(unavailable())
    }
}

pub(super) fn ensure_private_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
        if !meta.file_type().is_file()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o777 != 0o600
            || meta.nlink() != 1
        {
            return Err(unavailable());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(unavailable())
    }
}

pub(super) fn check_sidecars(path: &Path) -> Result<(), String> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        match std::fs::symlink_metadata(&sidecar) {
            Ok(meta) => {
                if suffix != "-journal" || meta.len() > MAX_DATABASE_BYTES {
                    return Err(unavailable());
                }
                ensure_private_file(&sidecar)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unavailable()),
        }
    }
    Ok(())
}

pub(super) fn check_resources(path: &Path) -> Result<(), String> {
    check_database_size(path)?;
    check_resources_with_free_bytes(path, RESERVED_BYTES)
}

fn check_database_size(path: &Path) -> Result<(), String> {
    let size = std::fs::metadata(path).map_err(|_| unavailable())?.len();
    if size > MAX_DATABASE_BYTES {
        Err("checkpoint_store_over_budget".to_owned())
    } else {
        Ok(())
    }
}

// statvfs field widths vary across Unix targets.
#[allow(clippy::useless_conversion)]
pub(super) fn check_resources_with_free_bytes(path: &Path, reserve: u64) -> Result<(), String> {
    #[cfg(unix)]
    {
        use rustix::fs::{fstatvfs, open, Mode, OFlags};
        let parent = path.parent().ok_or_else(unavailable)?;
        let fd = open(
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unavailable())?;
        let stats = fstatvfs(&fd).map_err(|_| unavailable())?;
        let free = u64::try_from(stats.f_bavail)
            .unwrap_or(0)
            .checked_mul(u64::try_from(stats.f_frsize).unwrap_or(0))
            .unwrap_or(0);
        if !has_space_reserve(free, reserve) {
            return Err("checkpoint_low_space".to_owned());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, reserve);
        Err(unavailable())
    }
}

fn unavailable() -> String {
    "checkpoint_store_unavailable".to_owned()
}

fn has_space_reserve(available: u64, reserve: u64) -> bool {
    available >= reserve
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn private_path() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        (temp, private.join("checkpoint.sqlite3"))
    }

    fn payload() -> Payload {
        Payload {
            version: 1,
            checkpoint_id: InventoryItemId::new(),
            root_label: "Folder".into(),
            page_number: 1,
            entries_seen: 2,
            root_stamp: RootStamp {
                device: 1,
                inode: 2,
                mode: 0o40700,
                size: 4096,
                nlink: 2,
                mtime: 1,
                mtime_nsec: 2,
                ctime: 3,
                ctime_nsec: 4,
                birth_seconds: 5,
                birth_nanos: 6,
            },
            exclusion_revision: RevisionId::new(),
            replay_names: vec![b"one".to_vec(), b"two".to_vec()],
        }
    }

    #[test]
    fn journal_symlinks_and_hardlinked_databases_are_refused() {
        let (_temp, path) = private_path();
        let mut journal = path.as_os_str().to_os_string();
        journal.push("-journal");
        let target = path.parent().unwrap().join("source");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, PathBuf::from(journal)).unwrap();
        assert!(Store::open(&path).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let (_temp2, path2) = private_path();
        let store = Store::open(&path2).unwrap();
        drop(store);
        std::fs::hard_link(&path2, path2.parent().unwrap().join("alias")).unwrap();
        assert!(Store::open(&path2).is_err());
    }

    #[test]
    fn oversized_saved_row_is_corrupt_not_an_empty_store_and_cannot_be_overwritten() {
        let (_temp, path) = private_path();
        let mut store = Store::open(&path).unwrap();
        let text = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        store
            .connection
            .execute(
                "INSERT INTO folder_checkpoint(singleton,payload) VALUES(1,?1)",
                [text],
            )
            .unwrap();
        assert!(store.get().is_err());
        assert!(store.save(None, payload()).is_err());
        let length: i64 = store
            .connection
            .query_row("SELECT length(payload) FROM folder_checkpoint", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(length, MAX_PAYLOAD_BYTES as i64 + 1);
    }

    #[test]
    fn explicit_save_reopens_and_cas_protects_the_single_row() {
        let (_temp, path) = private_path();
        let first = payload();
        let mut store = Store::open(&path).unwrap();
        assert!(store.get().unwrap().is_none());
        let info = store.save(None, first.clone()).unwrap();
        assert_eq!(info.checkpoint_id, first.checkpoint_id);
        assert_eq!(store.save(None, payload()).unwrap_err(), "stale_checkpoint");
        drop(store);
        let mut reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.get().unwrap(), Some(first.clone()));
        let next = payload();
        reopened
            .save(Some(first.checkpoint_id), next.clone())
            .unwrap();
        assert_eq!(
            reopened.clear(first.checkpoint_id).unwrap_err(),
            "stale_checkpoint"
        );
        reopened.clear(next.checkpoint_id).unwrap();
        assert!(reopened.get().unwrap().is_none());
    }

    #[test]
    fn unsupported_schema_and_corrupt_payload_fail_closed() {
        let (_temp, path) = private_path();
        let store = Store::open(&path).unwrap();
        store
            .connection
            .execute("PRAGMA user_version=99", [])
            .unwrap();
        drop(store);
        assert_eq!(
            Store::open(&path).err().unwrap(),
            "checkpoint_store_unsupported"
        );

        let (_temp, path) = private_path();
        let mut store = Store::open(&path).unwrap();
        let value = payload();
        store.save(None, value).unwrap();
        store
            .connection
            .execute(
                "UPDATE folder_checkpoint SET payload='{' WHERE singleton=1",
                [],
            )
            .unwrap();
        assert_eq!(store.get().unwrap_err(), "checkpoint_store_corrupt");
    }

    #[test]
    fn rejects_invalid_or_oversized_raw_names_and_low_space() {
        let mut value = payload();
        value.replay_names[1] = b"../escape".to_vec();
        assert_eq!(validate_payload(&value).unwrap_err(), "checkpoint_invalid");
        value.replay_names[1] = vec![b'x'; 256];
        assert_eq!(validate_payload(&value).unwrap_err(), "checkpoint_invalid");
        assert!(!has_space_reserve(RESERVED_BYTES - 1, RESERVED_BYTES));
        assert!(has_space_reserve(RESERVED_BYTES, RESERVED_BYTES));
    }
}
