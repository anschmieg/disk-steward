//! Private persistence for confirmed, exact-directory exclusion bindings.
//!
//! The store never observes or traverses the bound directory. It handles only its
//! own SQLite database. Stored bindings do not grant access or imply that a rule
//! matched a scan; matching must be re-established by a native adapter.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use disk_steward_core::{GlobalExclusionId, RevisionId};
use rusqlite::{params, Connection, OpenFlags, Transaction, TransactionBehavior};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_ENTRIES: usize = 256;
pub const MAX_LOCATOR_BYTES: usize = 16 * 1024;
pub const MAX_IDENTITY_BYTES: usize = 1024;
pub const MAX_BASENAME_BYTES: usize = 512;
pub const SCHEMA_VERSION: i64 = 1;
const APP_ID: i64 = 0x4453_4558; // DSEX
const MAX_ROWS: i64 = MAX_ENTRIES as i64;
const MAX_BINDING_BLOB_BYTES: usize = MAX_LOCATOR_BYTES + 2 * MAX_IDENTITY_BYTES;
const MAX_TYPED_ID_BYTES: usize = 128;
const MAX_SCHEMA_SQL_BYTES: usize = 2048;
const SCHEMA: &[(&str, &str)] = &[
    (
        "directory_exclusions",
        "CREATE TABLE directory_exclusions(exclusion_id TEXT PRIMARY KEY,encoding_version INTEGER NOT NULL,locator_bytes BLOB NOT NULL UNIQUE,volume_identity BLOB NOT NULL,directory_identity BLOB NOT NULL,basename TEXT NOT NULL,binding_sha256 TEXT NOT NULL) STRICT",
    ),
    (
        "registry_metadata",
        "CREATE TABLE registry_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),registry_revision TEXT NOT NULL) STRICT",
    ),
];

/// A natively selected directory binding. Its locator and identity bytes are
/// private backend material: this type deliberately has no `Debug` or Serde impl.
#[derive(Clone, PartialEq, Eq)]
pub struct NativeBinding {
    encoding_version: u32,
    locator_bytes: Vec<u8>,
    volume_identity: Vec<u8>,
    directory_identity: Vec<u8>,
    basename: String,
}

impl NativeBinding {
    /// Creates a validated v1 binding from a trusted native adapter observation.
    /// This constructor does not read or validate the selected filesystem path.
    pub fn new(
        locator_bytes: Vec<u8>,
        volume_identity: Vec<u8>,
        directory_identity: Vec<u8>,
        basename: String,
    ) -> StoreResult<Self> {
        if locator_bytes.is_empty()
            || locator_bytes.len() > MAX_LOCATOR_BYTES
            || locator_bytes.first() != Some(&b'/')
            || locator_bytes.contains(&0)
            || volume_identity.is_empty()
            || volume_identity.len() > MAX_IDENTITY_BYTES
            || directory_identity.is_empty()
            || directory_identity.len() > MAX_IDENTITY_BYTES
            || basename.is_empty()
            || basename.len() > MAX_BASENAME_BYTES
            || basename.chars().any(char::is_control)
            || basename.chars().any(is_bidi_control)
            || basename.contains('/')
            || basename == "."
            || basename == ".."
            || basename.trim() != basename
        {
            return Err(RegistryError::InvalidBinding);
        }
        Ok(Self {
            encoding_version: 1,
            locator_bytes,
            volume_identity,
            directory_identity,
            basename,
        })
    }

    /// Returns the opaque, lossless location locator for native confirmation and
    /// identity comparison. Do not serialize, log, or return it to the renderer.
    #[must_use]
    pub fn locator_bytes(&self) -> &[u8] {
        &self.locator_bytes
    }

    /// Returns the opaque volume identity bytes for native comparison only.
    #[must_use]
    pub fn volume_identity(&self) -> &[u8] {
        &self.volume_identity
    }

    /// Returns the opaque directory identity bytes for native comparison only.
    #[must_use]
    pub fn directory_identity(&self) -> &[u8] {
        &self.directory_identity
    }

    /// Returns the bounded basename for native confirmation and public display.
    #[must_use]
    pub fn basename(&self) -> &str {
        &self.basename
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// The store has not established a fresh native match. This is the only
    /// status emitted by the registry itself.
    SavedUnverified,
    /// A caller observed a stale binding. This value is not persisted here.
    Stale,
    /// A caller could not access or identify the location. This is not persisted.
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExclusionEntry {
    pub id: GlobalExclusionId,
    pub basename: String,
    pub status: EntryStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrySnapshot {
    pub schema_version: u32,
    pub revision_id: RevisionId,
    pub entries: Vec<ExclusionEntry>,
}

pub enum RegistryError {
    StaleRevision,
    UnsupportedSchema,
    CorruptData,
    InvalidBinding,
    BindingConflict,
    EntryNotFound,
    LimitReached,
    PrivacyUnavailable,
    Storage,
}

impl RegistryError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleRevision => "stale_revision",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::CorruptData => "corrupt_data",
            Self::InvalidBinding => "invalid_binding",
            Self::BindingConflict => "binding_conflict",
            Self::EntryNotFound => "entry_not_found",
            Self::LimitReached => "registry_limit",
            Self::PrivacyUnavailable => "privacy_unavailable",
            Self::Storage => "storage_error",
        }
    }
}

impl std::fmt::Debug for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for RegistryError {}

impl From<rusqlite::Error> for RegistryError {
    fn from(_error: rusqlite::Error) -> Self {
        Self::Storage
    }
}

pub type StoreResult<T> = Result<T, RegistryError>;

/// SQLite-backed exact-directory binding registry.
///
/// `add_confirmed` and `remove_confirmed` are intended only for a native caller
/// that has already shown and received a separate native confirmation. The store
/// cannot prove that UI event occurred; renderer IPC must never call these APIs.
pub struct Registry {
    connection: Connection,
}

#[derive(Clone)]
struct StoredEntry {
    id: GlobalExclusionId,
    binding: NativeBinding,
}

#[derive(Serialize)]
struct BindingManifest<'a> {
    exclusion_id: GlobalExclusionId,
    encoding_version: u32,
    locator_bytes: &'a [u8],
    volume_identity: &'a [u8],
    directory_identity: &'a [u8],
    basename: &'a str,
}

impl Registry {
    /// Opens or initializes a registry at a private app-owned database path.
    /// Existing paths are never chmodded; insufficient privacy causes failure.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = private_database_path(path.as_ref())?;
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        initialize(&mut connection)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { connection })
    }

    /// Returns bounded public rows only. Every entry is unverified by default.
    pub fn snapshot(&self) -> StoreResult<RegistrySnapshot> {
        let tx = self.connection.unchecked_transaction()?;
        let (revision_id, rows) = load_registry(&tx)?;
        let snapshot = public_snapshot(revision_id, rows);
        tx.commit()?;
        Ok(snapshot)
    }

    /// Returns private binding data to the trusted native adapter only. This DTO
    /// has no serialization/debug support and is not suitable for renderer IPC.
    pub fn get_binding(&self, id: GlobalExclusionId) -> StoreResult<NativeBinding> {
        let tx = self.connection.unchecked_transaction()?;
        let (_, rows) = load_registry(&tx)?;
        let binding = rows
            .into_iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.binding)
            .ok_or(RegistryError::EntryNotFound)?;
        tx.commit()?;
        Ok(binding)
    }

    /// Adds a binding after the trusted native caller separately confirmed it.
    /// An exact duplicate is a semantic no-op; same-locator identity changes
    /// require removal of the saved binding before a new one can be added.
    pub fn add_confirmed(
        &mut self,
        expected_revision: RevisionId,
        binding: NativeBinding,
    ) -> StoreResult<RegistrySnapshot> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (actual_revision, mut rows) = load_registry(&tx)?;
        check_revision(actual_revision, expected_revision)?;
        if let Some(existing) = rows
            .iter()
            .find(|entry| entry.binding.locator_bytes == binding.locator_bytes)
        {
            if existing.binding.volume_identity == binding.volume_identity
                && existing.binding.directory_identity == binding.directory_identity
            {
                let snapshot = public_snapshot(actual_revision, rows);
                tx.commit()?;
                return Ok(snapshot);
            }
            return Err(RegistryError::BindingConflict);
        }
        if rows.len() >= MAX_ENTRIES {
            return Err(RegistryError::LimitReached);
        }
        let id = GlobalExclusionId::new();
        insert_entry(&tx, id, &binding)?;
        rows.push(StoredEntry { id, binding });
        let revision_id = RevisionId::new();
        save_revision(&tx, revision_id)?;
        tx.commit()?;
        Ok(public_snapshot(revision_id, rows))
    }

    /// Removes one binding after the native caller separately confirmed it.
    pub fn remove_confirmed(
        &mut self,
        expected_revision: RevisionId,
        id: GlobalExclusionId,
    ) -> StoreResult<RegistrySnapshot> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (actual_revision, mut rows) = load_registry(&tx)?;
        check_revision(actual_revision, expected_revision)?;
        let Some(index) = rows.iter().position(|entry| entry.id == id) else {
            return Err(RegistryError::EntryNotFound);
        };
        tx.execute(
            "DELETE FROM directory_exclusions WHERE exclusion_id=?1",
            [id_string(id)?],
        )?;
        rows.remove(index);
        let revision_id = RevisionId::new();
        save_revision(&tx, revision_id)?;
        tx.commit()?;
        Ok(public_snapshot(revision_id, rows))
    }
}

fn public_snapshot(revision_id: RevisionId, mut rows: Vec<StoredEntry>) -> RegistrySnapshot {
    rows.sort_by_key(|entry| entry.id);
    RegistrySnapshot {
        schema_version: 1,
        revision_id,
        entries: rows
            .into_iter()
            .map(|entry| ExclusionEntry {
                id: entry.id,
                basename: entry.binding.basename,
                status: EntryStatus::SavedUnverified,
            })
            .collect(),
    }
}

fn private_database_path(path: &Path) -> StoreResult<PathBuf> {
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(RegistryError::PrivacyUnavailable)
    }

    #[cfg(unix)]
    {
        use std::{
            fs::{self, OpenOptions},
            os::unix::fs::{MetadataExt, OpenOptionsExt},
        };

        let name = path.file_name().ok_or(RegistryError::PrivacyUnavailable)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent_meta =
            fs::symlink_metadata(parent).map_err(|_| RegistryError::PrivacyUnavailable)?;
        if !parent_meta.file_type().is_dir() || parent_meta.mode() & 0o077 != 0 {
            return Err(RegistryError::PrivacyUnavailable);
        }
        let canonical_parent =
            fs::canonicalize(parent).map_err(|_| RegistryError::PrivacyUnavailable)?;
        let candidate = canonical_parent.join(name);

        match fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() || metadata.mode() & 0o077 != 0 {
                    return Err(RegistryError::PrivacyUnavailable);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true).mode(0o600);
                match options.open(&candidate) {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = fs::symlink_metadata(&candidate)
                            .map_err(|_| RegistryError::PrivacyUnavailable)?;
                        if !metadata.file_type().is_file() || metadata.mode() & 0o077 != 0 {
                            return Err(RegistryError::PrivacyUnavailable);
                        }
                    }
                    Err(_) => return Err(RegistryError::PrivacyUnavailable),
                }
            }
            Err(_) => return Err(RegistryError::PrivacyUnavailable),
        }
        Ok(candidate)
    }
}

fn initialize(connection: &mut Connection) -> StoreResult<()> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let app_id: i64 = tx.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let objects = object_count(&tx)?;
    if app_id == 0 && version == 0 && objects == 0 {
        tx.execute_batch(SCHEMA[0].1)?;
        tx.execute_batch(SCHEMA[1].1)?;
        tx.pragma_update(None, "application_id", APP_ID)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        let revision_id = RevisionId::new();
        tx.execute(
            "INSERT INTO registry_metadata(singleton,registry_revision) VALUES(1,?1)",
            [id_string(revision_id)?],
        )?;
    }
    validate_schema(&tx)?;
    tx.commit()?;
    Ok(())
}

fn validate_schema(connection: &Connection) -> StoreResult<()> {
    let app_id: i64 = connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if app_id != APP_ID || version != SCHEMA_VERSION {
        return Err(RegistryError::UnsupportedSchema);
    }
    if object_count(connection)? != SCHEMA.len() as i64 {
        return Err(RegistryError::UnsupportedSchema);
    }
    let mut statement = connection.prepare(
        "SELECT type,name,sql FROM sqlite_master WHERE name NOT GLOB 'sqlite_*' AND length(CAST(name AS BLOB))<=?1 AND length(CAST(sql AS BLOB))<=?2 ORDER BY type,name",
    )?;
    let objects = statement
        .query_map(
            [MAX_TYPED_ID_BYTES as i64, MAX_SCHEMA_SQL_BYTES as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    if objects.len() != SCHEMA.len() {
        return Err(RegistryError::UnsupportedSchema);
    }
    for ((kind, name, sql), (expected_name, expected_sql)) in objects.iter().zip(SCHEMA) {
        if kind != "table"
            || name != expected_name
            || sql.as_deref().map(normalize).as_deref() != Some(normalize(expected_sql).as_str())
        {
            return Err(RegistryError::UnsupportedSchema);
        }
    }
    Ok(())
}

fn object_count(connection: &Connection) -> StoreResult<i64> {
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
        [],
        |row| row.get(0),
    )?)
}

fn normalize(sql: &str) -> String {
    sql.chars()
        .filter(|ch| !ch.is_ascii_whitespace() && *ch != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch as u32,
        0x061c | 0x200e | 0x200f | 0x202a..=0x202e | 0x2066..=0x2069
    )
}

fn load_registry(connection: &Connection) -> StoreResult<(RevisionId, Vec<StoredEntry>)> {
    validate_schema(connection)?;
    let revision: String = match connection.query_row(
        "SELECT registry_revision FROM registry_metadata WHERE singleton=1 AND length(CAST(registry_revision AS BLOB))<=?1",
        [MAX_TYPED_ID_BYTES as i64],
        |row| row.get(0),
    ) {
        Ok(revision) => revision,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Err(RegistryError::CorruptData),
        Err(error) => return Err(error.into()),
    };
    let revision_id = parse_id(&revision)?;
    let count: i64 =
        connection.query_row("SELECT COUNT(*) FROM directory_exclusions", [], |row| {
            row.get(0)
        })?;
    if !(0..=MAX_ROWS).contains(&count) {
        return Err(RegistryError::CorruptData);
    }
    let mut statement = connection.prepare(
        "SELECT exclusion_id,encoding_version,locator_bytes,volume_identity,directory_identity,basename,binding_sha256 FROM directory_exclusions WHERE length(CAST(exclusion_id AS BLOB))<=?1 AND length(locator_bytes)<=?2 AND length(volume_identity)<=?3 AND length(directory_identity)<=?3 AND length(CAST(basename AS BLOB))<=?4 AND length(CAST(binding_sha256 AS BLOB))<=?5 LIMIT ?6",
    )?;
    let rows = statement.query_map(
        params![
            MAX_TYPED_ID_BYTES as i64,
            MAX_LOCATOR_BYTES as i64,
            MAX_IDENTITY_BYTES as i64,
            MAX_BASENAME_BYTES as i64,
            64_i64,
            MAX_ROWS + 1
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        },
    )?;
    let mut entries = Vec::new();
    for row in rows {
        let (id, version, locator, volume, directory, basename, digest) = row?;
        if version != 1
            || locator.len() > MAX_LOCATOR_BYTES
            || volume.len() > MAX_IDENTITY_BYTES
            || directory.len() > MAX_IDENTITY_BYTES
            || locator.len() + volume.len() + directory.len() > MAX_BINDING_BLOB_BYTES
        {
            return Err(RegistryError::CorruptData);
        }
        let id: GlobalExclusionId = parse_id(&id)?;
        let binding = NativeBinding::new(locator, volume, directory, basename)
            .map_err(|_| RegistryError::CorruptData)?;
        if binding_digest(id, &binding)? != digest {
            return Err(RegistryError::CorruptData);
        }
        entries.push(StoredEntry { id, binding });
    }
    if entries.len() != count as usize {
        return Err(RegistryError::CorruptData);
    }
    Ok((revision_id, entries))
}

fn insert_entry(
    tx: &Transaction<'_>,
    id: GlobalExclusionId,
    binding: &NativeBinding,
) -> StoreResult<()> {
    tx.execute(
        "INSERT INTO directory_exclusions(exclusion_id,encoding_version,locator_bytes,volume_identity,directory_identity,basename,binding_sha256) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            id_string(id)?,
            i64::from(binding.encoding_version),
            binding.locator_bytes,
            binding.volume_identity,
            binding.directory_identity,
            binding.basename,
            binding_digest(id, binding)?
        ],
    )?;
    Ok(())
}

fn save_revision(tx: &Transaction<'_>, revision_id: RevisionId) -> StoreResult<()> {
    tx.execute(
        "UPDATE registry_metadata SET registry_revision=?1 WHERE singleton=1",
        [id_string(revision_id)?],
    )?;
    Ok(())
}

fn binding_digest(id: GlobalExclusionId, binding: &NativeBinding) -> StoreResult<String> {
    let manifest = BindingManifest {
        exclusion_id: id,
        encoding_version: binding.encoding_version,
        locator_bytes: &binding.locator_bytes,
        volume_identity: &binding.volume_identity,
        directory_identity: &binding.directory_identity,
        basename: &binding.basename,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|_| RegistryError::CorruptData)?;
    Ok(sha256(&bytes))
}

fn check_revision(actual: RevisionId, expected: RevisionId) -> StoreResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(RegistryError::StaleRevision)
    }
}

fn id_string<T: Serialize>(id: T) -> StoreResult<String> {
    serde_json::to_value(id)
        .map_err(|_| RegistryError::CorruptData)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or(RegistryError::CorruptData)
}

fn parse_id<T: DeserializeOwned>(id: &str) -> StoreResult<T> {
    serde_json::from_value(serde_json::Value::String(id.to_owned()))
        .map_err(|_| RegistryError::CorruptData)
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::{
        fs,
        sync::{Arc, Barrier},
        thread,
    };
    #[cfg(unix)]
    use tempfile::tempdir;

    #[cfg(unix)]
    fn database() -> (tempfile::TempDir, PathBuf) {
        let directory = tempdir().unwrap();
        let private = directory.path().join("private");
        fs::create_dir(&private).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = private.join("exclusions.sqlite3");
        (directory, path)
    }

    #[cfg(unix)]
    fn binding(locator: &[u8]) -> NativeBinding {
        NativeBinding::new(
            locator.to_vec(),
            b"volume-uuid".to_vec(),
            b"directory-resource-id".to_vec(),
            "Caches".to_owned(),
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn inserts_private_binding_and_returns_only_public_snapshot_fields() {
        let (_directory, path) = database();
        let mut registry = Registry::open(&path).unwrap();
        let initial = registry.snapshot().unwrap();
        assert_eq!(initial.schema_version, 1);
        assert!(initial.entries.is_empty());

        let saved = registry
            .add_confirmed(initial.revision_id, binding(b"/Volumes/Data/Caches"))
            .unwrap();
        assert_eq!(saved.entries.len(), 1);
        assert_eq!(saved.entries[0].basename, "Caches");
        assert_eq!(saved.entries[0].status, EntryStatus::SavedUnverified);
        let json = serde_json::to_string(&saved).unwrap();
        assert!(json.contains("schemaVersion"));
        assert!(json.contains("revisionId"));
        assert!(json.contains("saved_unverified"));
        assert!(!json.contains("Volumes"));
        assert!(!json.contains("volume-uuid"));
        assert!(!json.contains("directory-resource-id"));

        let stored = registry.get_binding(saved.entries[0].id).unwrap();
        assert_eq!(stored.locator_bytes(), b"/Volumes/Data/Caches");
        assert_eq!(stored.volume_identity(), b"volume-uuid");
        assert_eq!(stored.directory_identity(), b"directory-resource-id");
        assert_eq!(stored.basename(), "Caches");
    }

    #[cfg(unix)]
    #[test]
    fn exact_duplicate_is_noop_but_same_locator_with_new_identity_conflicts() {
        let (_directory, path) = database();
        let mut registry = Registry::open(&path).unwrap();
        let initial = registry.snapshot().unwrap();
        let first = registry
            .add_confirmed(initial.revision_id, binding(b"/Users/example/Caches"))
            .unwrap();
        let duplicate = registry
            .add_confirmed(first.revision_id, binding(b"/Users/example/Caches"))
            .unwrap();
        assert_eq!(duplicate, first);

        let replacement = NativeBinding::new(
            b"/Users/example/Caches".to_vec(),
            b"volume-uuid".to_vec(),
            b"replacement-directory-id".to_vec(),
            "Caches".to_owned(),
        )
        .unwrap();
        assert!(matches!(
            registry.add_confirmed(first.revision_id, replacement),
            Err(RegistryError::BindingConflict)
        ));
        assert_eq!(registry.snapshot().unwrap(), first);
    }

    #[cfg(unix)]
    #[test]
    fn stale_revision_and_removal_are_transactional() {
        let (_directory, path) = database();
        let mut registry = Registry::open(&path).unwrap();
        let initial = registry.snapshot().unwrap();
        let saved = registry
            .add_confirmed(initial.revision_id, binding(b"/Users/example/Library"))
            .unwrap();
        assert!(matches!(
            registry.add_confirmed(initial.revision_id, binding(b"/Users/example/Other")),
            Err(RegistryError::StaleRevision)
        ));
        assert_eq!(registry.snapshot().unwrap(), saved);

        let removed = registry
            .remove_confirmed(saved.revision_id, saved.entries[0].id)
            .unwrap();
        assert!(removed.entries.is_empty());
        assert_ne!(removed.revision_id, saved.revision_id);
        assert!(matches!(
            registry.remove_confirmed(saved.revision_id, saved.entries[0].id),
            Err(RegistryError::StaleRevision)
        ));
        assert_eq!(registry.snapshot().unwrap(), removed);
        assert!(matches!(
            registry.get_binding(saved.entries[0].id),
            Err(RegistryError::EntryNotFound)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn persists_across_reopen_and_roundtrips_non_utf8_locator_bytes() {
        let (_directory, path) = database();
        let mut registry = Registry::open(&path).unwrap();
        let initial = registry.snapshot().unwrap();
        let saved = registry
            .add_confirmed(initial.revision_id, binding(b"/Users/example/Cache-\xff"))
            .unwrap();
        drop(registry);

        let reopened = Registry::open(&path).unwrap();
        assert_eq!(reopened.snapshot().unwrap(), saved);
        assert_eq!(
            reopened
                .get_binding(saved.entries[0].id)
                .unwrap()
                .locator_bytes(),
            b"/Users/example/Cache-\xff"
        );
    }

    #[test]
    fn rejects_invalid_bounds_and_identity_material() {
        assert!(matches!(
            NativeBinding::new(vec![], b"v".to_vec(), b"d".to_vec(), "name".into()),
            Err(RegistryError::InvalidBinding)
        ));
        assert!(matches!(
            NativeBinding::new(
                b"relative/path".to_vec(),
                b"v".to_vec(),
                b"d".to_vec(),
                "name".into()
            ),
            Err(RegistryError::InvalidBinding)
        ));
        assert!(matches!(
            NativeBinding::new(
                b"/dir".to_vec(),
                vec![0; MAX_IDENTITY_BYTES + 1],
                b"d".to_vec(),
                "name".into()
            ),
            Err(RegistryError::InvalidBinding)
        ));
        assert!(matches!(
            NativeBinding::new(
                b"/dir".to_vec(),
                b"v".to_vec(),
                b"d".to_vec(),
                "unsafe\u{202e}name".into()
            ),
            Err(RegistryError::InvalidBinding)
        ));
        assert!(matches!(
            NativeBinding::new(
                b"/dir".to_vec(),
                b"v".to_vec(),
                b"d".to_vec(),
                "bad\nname".into()
            ),
            Err(RegistryError::InvalidBinding)
        ));
        assert!(matches!(
            NativeBinding::new(
                b"/dir".to_vec(),
                b"v".to_vec(),
                b"d".to_vec(),
                "x".repeat(MAX_BASENAME_BYTES + 1)
            ),
            Err(RegistryError::InvalidBinding)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn enforces_entry_limit_without_mutating_snapshot() {
        let (_directory, path) = database();
        let mut registry = Registry::open(&path).unwrap();
        let mut snapshot = registry.snapshot().unwrap();
        for index in 0..MAX_ENTRIES {
            snapshot = registry
                .add_confirmed(
                    snapshot.revision_id,
                    binding(format!("/Users/example/dir-{index}").as_bytes()),
                )
                .unwrap();
        }
        let before = snapshot.clone();
        assert!(matches!(
            registry.add_confirmed(before.revision_id, binding(b"/Users/example/overflow")),
            Err(RegistryError::LimitReached)
        ));
        assert_eq!(registry.snapshot().unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn unknown_schema_and_digest_tampering_are_rejected_without_reset() {
        let (_directory, path) = database();
        {
            let mut registry = Registry::open(&path).unwrap();
            let snapshot = registry.snapshot().unwrap();
            registry
                .add_confirmed(snapshot.revision_id, binding(b"/Users/example/dir"))
                .unwrap();
        }
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE unknown_user_data(secret TEXT); INSERT INTO unknown_user_data VALUES('preserve me')")
            .unwrap();
        drop(connection);
        assert!(matches!(
            Registry::open(&path),
            Err(RegistryError::UnsupportedSchema)
        ));
        let connection = Connection::open(&path).unwrap();
        let unknown_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM unknown_user_data", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(unknown_count, 1);
        let preserved: String = connection
            .query_row("SELECT secret FROM unknown_user_data", [], |row| row.get(0))
            .unwrap();
        assert_eq!(preserved, "preserve me");

        let (_directory, tampered_path) = database();
        let mut registry = Registry::open(&tampered_path).unwrap();
        let initial = registry.snapshot().unwrap();
        registry
            .add_confirmed(initial.revision_id, binding(b"/Users/example/integrity"))
            .unwrap();
        drop(registry);
        let connection = Connection::open(&tampered_path).unwrap();
        connection
            .execute("UPDATE directory_exclusions SET basename='Other'", [])
            .unwrap();
        drop(connection);
        let registry = Registry::open(&tampered_path).unwrap();
        assert!(matches!(
            registry.snapshot(),
            Err(RegistryError::CorruptData)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_oversized_rows_before_loading_blob() {
        let (_directory, path) = database();
        {
            Registry::open(&path).unwrap();
        }
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO directory_exclusions VALUES(?1,1,?2,?3,?4,?5,?6)",
                params![
                    id_string(GlobalExclusionId::new()).unwrap(),
                    vec![b'x'; MAX_LOCATOR_BYTES + 1],
                    b"v".to_vec(),
                    b"d".to_vec(),
                    "name",
                    "bad-digest"
                ],
            )
            .unwrap();
        drop(connection);
        let opened = Registry::open(&path).unwrap();
        assert!(matches!(opened.snapshot(), Err(RegistryError::CorruptData)));

        let (_directory, oversized_id_path) = database();
        Registry::open(&oversized_id_path).unwrap();
        let connection = Connection::open(&oversized_id_path).unwrap();
        connection
            .execute(
                "INSERT INTO directory_exclusions VALUES(?1,1,?2,?3,?4,?5,?6)",
                params![
                    "x".repeat(MAX_TYPED_ID_BYTES + 1),
                    b"/absolute/path".to_vec(),
                    b"v".to_vec(),
                    b"d".to_vec(),
                    "name",
                    "0".repeat(64)
                ],
            )
            .unwrap();
        drop(connection);
        assert!(matches!(
            Registry::open(&oversized_id_path).unwrap().snapshot(),
            Err(RegistryError::CorruptData)
        ));

        let (_directory, oversized_revision_path) = database();
        Registry::open(&oversized_revision_path).unwrap();
        let connection = Connection::open(&oversized_revision_path).unwrap();
        connection
            .execute(
                "UPDATE registry_metadata SET registry_revision=?1",
                ["x".repeat(MAX_TYPED_ID_BYTES + 1)],
            )
            .unwrap();
        drop(connection);
        assert!(matches!(
            Registry::open(&oversized_revision_path).unwrap().snapshot(),
            Err(RegistryError::CorruptData)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_leaf_and_insecure_parent_are_refused_without_chmodding() {
        let (_directory, path) = database();
        Registry::open(&path).unwrap();
        let target = path.with_file_name("target.sqlite3");
        fs::write(&target, b"keep target").unwrap();
        let symlink = path.with_file_name("link.sqlite3");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &symlink).unwrap();
        #[cfg(unix)]
        {
            assert!(matches!(
                Registry::open(&symlink),
                Err(RegistryError::PrivacyUnavailable)
            ));
            assert_eq!(fs::read(&target).unwrap(), b"keep target");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let parent = tempdir().unwrap();
            fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let unsafe_db = parent.path().join("not-created.sqlite3");
            assert!(matches!(
                Registry::open(&unsafe_db),
                Err(RegistryError::PrivacyUnavailable)
            ));
            assert!(!unsafe_db.exists());
            assert_eq!(
                fs::metadata(parent.path()).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn creates_database_with_private_permissions_and_rejects_insecure_existing_file() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, path) = database();
        let mut registry = Registry::open(&path).unwrap();
        let snapshot = registry.snapshot().unwrap();
        registry
            .add_confirmed(snapshot.revision_id, binding(b"/Users/example/private"))
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        assert_eq!(fs::metadata(wal).unwrap().permissions().mode() & 0o077, 0);
        drop(registry);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            Registry::open(&path),
            Err(RegistryError::PrivacyUnavailable)
        ));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn simultaneous_writers_have_one_revision_winner() {
        let (_directory, path) = database();
        let registry = Registry::open(&path).unwrap();
        let expected = registry.snapshot().unwrap().revision_id;
        drop(registry);
        let barrier = Arc::new(Barrier::new(3));
        let writers: Vec<_> = (0..2)
            .map(|index| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let mut registry = Registry::open(path).unwrap();
                    barrier.wait();
                    registry.add_confirmed(
                        expected,
                        binding(format!("/Users/example/concurrent-{index}").as_bytes()),
                    )
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(RegistryError::StaleRevision)))
                .count(),
            1
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn fails_closed_when_private_permissions_cannot_be_verified() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            Registry::open(directory.path().join("exclusions.sqlite3")),
            Err(RegistryError::PrivacyUnavailable)
        ));
    }
}
