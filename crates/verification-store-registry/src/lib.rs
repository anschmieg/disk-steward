//! Durable bookkeeping for verification-store identity and lifecycle.
//!
//! This is an index only. It grants no access to stores and performs no
//! store discovery, adoption, inspection, or cleanup.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use rustix::{
    fs::{flock, FlockOperation},
    process::geteuid,
};
use std::path::Path;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::PathBuf;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::Duration,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
const APP_ID: i64 = 0x4453_5652; // DSVR
#[cfg(any(target_os = "linux", target_os = "macos"))]
const SCHEMA_VERSION: i64 = 2;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_STORE_RESERVATION: u64 = 2 * 1024 * 1024 * 1024;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_TOTAL_RESERVATION: u64 = 4 * 1024 * 1024 * 1024;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_RECORDS: i64 = 4096;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_PAGE: usize = 256;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const V1_TABLE_SCHEMA: &str = "CREATE TABLE verification_stores (
    store_id TEXT PRIMARY KEY NOT NULL CHECK (length(CAST(store_id AS BLOB)) = 32 AND instr(store_id,char(0)) = 0 AND store_id NOT GLOB '*[^0-9a-f]*'),
    parent_dev TEXT NOT NULL CHECK (length(CAST(parent_dev AS BLOB)) BETWEEN 1 AND 20 AND parent_dev NOT GLOB '*[^0-9]*'),
    parent_ino TEXT NOT NULL CHECK (length(CAST(parent_ino AS BLOB)) BETWEEN 1 AND 20 AND parent_ino NOT GLOB '*[^0-9]*'),
    root_dev TEXT NOT NULL CHECK (length(CAST(root_dev AS BLOB)) BETWEEN 1 AND 20 AND root_dev NOT GLOB '*[^0-9]*'),
    root_ino TEXT NOT NULL CHECK (length(CAST(root_ino AS BLOB)) BETWEEN 1 AND 20 AND root_ino NOT GLOB '*[^0-9]*'),
    state TEXT NOT NULL CHECK (state IN ('registered','active','stopped','cleanup_pending','cleanup_complete','needs_reconciliation')),
    prior_state TEXT CHECK (prior_state IS NULL OR prior_state IN ('registered','active','stopped','cleanup_pending')),
    CHECK ((state = 'needs_reconciliation' AND prior_state IS NOT NULL AND prior_state IN ('registered','active','stopped','cleanup_pending')) OR (state != 'needs_reconciliation' AND prior_state IS NULL))
)";
#[cfg(any(target_os = "linux", target_os = "macos"))]
const TABLE_SCHEMA: &str = "CREATE TABLE verification_stores (
    store_id TEXT PRIMARY KEY NOT NULL CHECK (length(CAST(store_id AS BLOB)) = 32 AND instr(store_id,char(0)) = 0 AND store_id NOT GLOB '*[^0-9a-f]*'),
    parent_dev TEXT NOT NULL CHECK (length(CAST(parent_dev AS BLOB)) BETWEEN 1 AND 20 AND parent_dev NOT GLOB '*[^0-9]*'),
    parent_ino TEXT NOT NULL CHECK (length(CAST(parent_ino AS BLOB)) BETWEEN 1 AND 20 AND parent_ino NOT GLOB '*[^0-9]*'),
    root_dev TEXT NOT NULL CHECK (length(CAST(root_dev AS BLOB)) BETWEEN 1 AND 20 AND root_dev NOT GLOB '*[^0-9]*'),
    root_ino TEXT NOT NULL CHECK (length(CAST(root_ino AS BLOB)) BETWEEN 1 AND 20 AND root_ino NOT GLOB '*[^0-9]*'),
    state TEXT NOT NULL CHECK (state IN ('registered','active','stopped','cleanup_pending','cleanup_complete','needs_reconciliation')),
    prior_state TEXT CHECK (prior_state IS NULL OR prior_state IN ('registered','active','stopped','cleanup_pending')),
    reserved_bytes TEXT NOT NULL DEFAULT '0' CHECK (length(CAST(reserved_bytes AS BLOB)) BETWEEN 1 AND 20 AND reserved_bytes NOT GLOB '*[^0-9]*'),
    CHECK ((state = 'needs_reconciliation' AND prior_state IS NOT NULL AND prior_state IN ('registered','active','stopped','cleanup_pending')) OR (state != 'needs_reconciliation' AND prior_state IS NULL))
)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreState {
    Registered,
    Active,
    Stopped,
    CleanupPending,
    CleanupComplete,
    NeedsReconciliation,
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl StoreState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Active => "active",
            Self::Stopped => "stopped",
            Self::CleanupPending => "cleanup_pending",
            Self::CleanupComplete => "cleanup_complete",
            Self::NeedsReconciliation => "needs_reconciliation",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "registered" => Self::Registered,
            "active" => Self::Active,
            "stopped" => Self::Stopped,
            "cleanup_pending" => Self::CleanupPending,
            "cleanup_complete" => Self::CleanupComplete,
            "needs_reconciliation" => Self::NeedsReconciliation,
            _ => return None,
        })
    }
    fn allows(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Registered, Self::Active)
                | (Self::Active, Self::Stopped)
                | (Self::Stopped, Self::CleanupPending)
                | (Self::CleanupPending, Self::CleanupComplete)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreRecord {
    pub store_id: String,
    pub parent_dev: u64,
    pub parent_ino: u64,
    pub root_dev: u64,
    pub root_ino: u64,
    pub state: StoreState,
    pub prior_state: Option<StoreState>,
    /// `u64::MAX` marks a migrated v1 row whose prior usage is unknown.
    pub reserved_bytes: u64,
}

#[derive(Debug)]
pub enum RegistryError {
    InvalidInput,
    UnsafePath,
    UnsupportedPlatform,
    UnsupportedSchema,
    Capacity,
    ReservationLimit,
    ReconciliationRequired,
    Duplicate,
    NotFound,
    StaleState,
    InvalidTransition,
    Storage,
    LeaseHeld,
}
impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid registry input",
            Self::UnsafePath => "registry path is not private and safe",
            Self::UnsupportedPlatform => "registry platform is unsupported",
            Self::UnsupportedSchema => "registry schema is unknown or unsupported",
            Self::Capacity => "registry capacity reached",
            Self::ReservationLimit => "reservation budget exceeded",
            Self::ReconciliationRequired => "a store needs reconciliation",
            Self::Duplicate => "store ID already exists",
            Self::NotFound => "store does not exist",
            Self::StaleState => "store state changed",
            Self::InvalidTransition => "store state transition is invalid",
            Self::Storage => "registry storage error",
            Self::LeaseHeld => "registry is already open",
        })
    }
}
impl std::error::Error for RegistryError {}
/// Exclusive owner of the registry index. The sibling lease is held for its lifetime.
pub struct VerificationStoreRegistry {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    connection: Connection,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    _lease: File,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    lease_path: PathBuf,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    database_path: PathBuf,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    parent_path: PathBuf,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    parent_identity: (u64, u64, u32),
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    database_identity: (u64, u64, u32),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl VerificationStoreRegistry {
    /// Open a trusted backend path within an existing mode-0700 directory.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RegistryError> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = path;
            return Err(RegistryError::UnsupportedPlatform);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let path = path.as_ref();
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or(RegistryError::UnsafePath)?;
            validate_private_dir(parent)?;
            // Resolve only the validated directory. Keep the database leaf
            // unresolved so SQLite's NOFOLLOW flag still rejects a symlink.
            let canonical_parent = parent
                .canonicalize()
                .map_err(|_| RegistryError::UnsafePath)?;
            validate_private_dir(&canonical_parent)?;
            let canonical_path =
                canonical_parent.join(path.file_name().ok_or(RegistryError::UnsafePath)?);
            let parent = canonical_parent.as_path();
            let path = canonical_path.as_path();
            let lease_path = sibling(path, ".lease")?;
            let lease = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(nofollow_flag() | nonblock_flag())
                .open(&lease_path)
                .map_err(|_| RegistryError::UnsafePath)?;
            validate_private_file(&lease)?;
            flock(&lease, FlockOperation::NonBlockingLockExclusive)
                .map_err(|_| RegistryError::LeaseHeld)?;
            validate_database(path)?;
            let database_before =
                fs::symlink_metadata(path).map_err(|_| RegistryError::UnsafePath)?;
            validate_metadata_file(&database_before)?;
            let mut connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|_| RegistryError::Storage)?;
            let database_opened =
                fs::symlink_metadata(path).map_err(|_| RegistryError::UnsafePath)?;
            validate_metadata_file(&database_opened)?;
            if (database_before.dev(), database_before.ino())
                != (database_opened.dev(), database_opened.ino())
            {
                return Err(RegistryError::UnsafePath);
            }
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(|_| RegistryError::Storage)?;
            connection
                .pragma_update(None, "synchronous", "FULL")
                .map_err(|_| RegistryError::Storage)?;
            let parent_meta =
                fs::symlink_metadata(parent).map_err(|_| RegistryError::UnsafePath)?;
            let parent_identity = (parent_meta.dev(), parent_meta.ino(), parent_meta.uid());
            initialize_and_classify(&mut connection)?;
            let database_meta =
                fs::symlink_metadata(path).map_err(|_| RegistryError::UnsafePath)?;
            validate_metadata_file(&database_meta)?;
            if (database_opened.dev(), database_opened.ino())
                != (database_meta.dev(), database_meta.ino())
            {
                return Err(RegistryError::UnsafePath);
            }
            Ok(Self {
                connection,
                _lease: lease,
                lease_path,
                database_path: path.to_path_buf(),
                parent_path: parent.to_path_buf(),
                parent_identity,
                database_identity: (
                    database_meta.dev(),
                    database_meta.ino(),
                    database_meta.uid(),
                ),
            })
        }
    }

    /// Insert immutable identity metadata. IDs are generated by the trusted caller.
    pub fn register(
        &mut self,
        store_id: &str,
        parent_dev: u64,
        parent_ino: u64,
        root_dev: u64,
        root_ino: u64,
    ) -> Result<StoreRecord, RegistryError> {
        self.validate_lease()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| RegistryError::Storage)?;
        validate_schema(&tx)?;
        validate_records(&tx)?;
        if store_id.len() != 32
            || !store_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(RegistryError::InvalidInput);
        }
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM (SELECT 1 FROM verification_stores LIMIT 4097)",
                [],
                |r| r.get(0),
            )
            .map_err(|_| RegistryError::Storage)?;
        if count >= MAX_RECORDS {
            return Err(RegistryError::Capacity);
        }
        tx.execute("INSERT INTO verification_stores(store_id,parent_dev,parent_ino,root_dev,root_ino,state,prior_state) VALUES(?1,?2,?3,?4,?5,'registered',NULL)", params![store_id,parent_dev.to_string(),parent_ino.to_string(),root_dev.to_string(),root_ino.to_string()]).map_err(|e| if is_constraint(&e) { RegistryError::Duplicate } else { RegistryError::Storage })?;
        tx.commit().map_err(|_| RegistryError::Storage)?;
        Ok(StoreRecord {
            store_id: store_id.into(),
            parent_dev,
            parent_ino,
            root_dev,
            root_ino,
            state: StoreState::Registered,
            prior_state: None,
            reserved_bytes: 0,
        })
    }

    /// Store a monotonic admission reservation as metadata, not an allocation claim.
    pub fn reserve(
        &mut self,
        id: &str,
        expected_current: u64,
        new_total: u64,
    ) -> Result<StoreRecord, RegistryError> {
        self.validate_lease()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| RegistryError::Storage)?;
        validate_schema(&tx)?;
        validate_records(&tx)?;
        validate_id(id)?;
        if new_total < expected_current {
            return Err(RegistryError::InvalidInput);
        }
        if new_total > MAX_STORE_RESERVATION {
            return Err(RegistryError::ReservationLimit);
        }
        let blocked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM verification_stores WHERE state='needs_reconciliation' OR reserved_bytes=?1)", [u64::MAX.to_string()], |r| r.get(0)).map_err(|_| RegistryError::Storage)?;
        if blocked {
            return Err(RegistryError::ReconciliationRequired);
        }
        let found: Option<(String, String)> = tx
            .query_row(
                "SELECT reserved_bytes,state FROM verification_stores WHERE store_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| RegistryError::Storage)?;
        let (current_s, state) = found.ok_or(RegistryError::NotFound)?;
        let current = current_s
            .parse::<u64>()
            .map_err(|_| RegistryError::UnsupportedSchema)?;
        if current != expected_current {
            return Err(RegistryError::StaleState);
        }
        if !matches!(state.as_str(), "registered" | "active") {
            return Err(RegistryError::InvalidTransition);
        }
        let mut total = 0u64;
        let mut stmt = tx
            .prepare("SELECT reserved_bytes FROM verification_stores")
            .map_err(|_| RegistryError::Storage)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| RegistryError::Storage)?;
        for row in rows {
            total = total
                .checked_add(
                    row.map_err(|_| RegistryError::Storage)?
                        .parse::<u64>()
                        .map_err(|_| RegistryError::UnsupportedSchema)?,
                )
                .ok_or(RegistryError::ReservationLimit)?;
        }
        drop(stmt);
        let after = total
            .checked_sub(current)
            .and_then(|v| v.checked_add(new_total))
            .ok_or(RegistryError::ReservationLimit)?;
        if after > MAX_TOTAL_RESERVATION {
            return Err(RegistryError::ReservationLimit);
        }
        tx.execute(
            "UPDATE verification_stores SET reserved_bytes=?2 WHERE store_id=?1",
            params![id, new_total.to_string()],
        )
        .map_err(|_| RegistryError::Storage)?;
        let record=tx.query_row("SELECT store_id,parent_dev,parent_ino,root_dev,root_ino,state,prior_state,reserved_bytes FROM verification_stores WHERE store_id=?1",[id],read_record).map_err(|_|RegistryError::Storage)?;
        tx.commit().map_err(|_| RegistryError::Storage)?;
        Ok(record)
    }

    /// Compare-and-transition bookkeeping state; this grants no operational permission.
    pub fn transition(
        &mut self,
        id: &str,
        expected: StoreState,
        next: StoreState,
    ) -> Result<StoreRecord, RegistryError> {
        self.validate_lease()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| RegistryError::Storage)?;
        validate_schema(&tx)?;
        validate_records(&tx)?;
        validate_id(id)?;
        if !expected.allows(next) {
            return Err(RegistryError::InvalidTransition);
        }
        let changed = tx.execute("UPDATE verification_stores SET state=?3, prior_state=NULL WHERE store_id=?1 AND state=?2", params![id,expected.as_str(),next.as_str()]).map_err(|_| RegistryError::Storage)?;
        if changed == 0 {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM verification_stores WHERE store_id=?1)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(|_| RegistryError::Storage)?;
            return Err(if exists {
                RegistryError::StaleState
            } else {
                RegistryError::NotFound
            });
        }
        let record = tx.query_row("SELECT store_id,parent_dev,parent_ino,root_dev,root_ino,state,prior_state,reserved_bytes FROM verification_stores WHERE store_id=?1", [id], read_record).map_err(|_| RegistryError::Storage)?;
        tx.commit().map_err(|_| RegistryError::Storage)?;
        Ok(record)
    }

    pub fn get(&self, id: &str) -> Result<Option<StoreRecord>, RegistryError> {
        self.validate_lease()?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|_| RegistryError::Storage)?;
        validate_schema(&tx)?;
        validate_records(&tx)?;
        validate_id(id)?;
        let record = tx.query_row("SELECT store_id,parent_dev,parent_ino,root_dev,root_ino,state,prior_state,reserved_bytes FROM verification_stores WHERE store_id=?1", [id], read_record).optional().map_err(|_| RegistryError::Storage)?;
        tx.commit().map_err(|_| RegistryError::Storage)?;
        Ok(record)
    }

    /// Bounded keyset page, at most 256 records. `after` is an opaque store ID cursor.
    pub fn list(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoreRecord>, RegistryError> {
        self.validate_lease()?;
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|_| RegistryError::Storage)?;
        validate_schema(&tx)?;
        validate_records(&tx)?;
        if limit == 0 || limit > MAX_PAGE {
            return Err(RegistryError::InvalidInput);
        }
        if let Some(id) = after {
            validate_id(id)?;
        }
        let mut statement = tx.prepare("SELECT store_id,parent_dev,parent_ino,root_dev,root_ino,state,prior_state,reserved_bytes FROM verification_stores WHERE (?1 IS NULL OR store_id>?1) ORDER BY store_id LIMIT ?2").map_err(|_| RegistryError::Storage)?;
        let rows = statement
            .query_map(params![after, limit as i64], read_record)
            .map_err(|_| RegistryError::Storage)?;
        let result = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| RegistryError::Storage)?;
        drop(statement);
        tx.commit().map_err(|_| RegistryError::Storage)?;
        Ok(result)
    }

    fn validate_lease(&self) -> Result<(), RegistryError> {
        validate_private_file(&self._lease)?;
        validate_private_dir(&self.parent_path)?;
        let parent =
            fs::symlink_metadata(&self.parent_path).map_err(|_| RegistryError::UnsafePath)?;
        if (parent.dev(), parent.ino(), parent.uid()) != self.parent_identity {
            return Err(RegistryError::UnsafePath);
        }
        let opened = self
            ._lease
            .metadata()
            .map_err(|_| RegistryError::UnsafePath)?;
        let named =
            fs::symlink_metadata(&self.lease_path).map_err(|_| RegistryError::UnsafePath)?;
        if named.file_type().is_symlink()
            || !named.is_file()
            || named.dev() != opened.dev()
            || named.ino() != opened.ino()
            || named.uid() != geteuid().as_raw()
            || named.mode() & 0o7777 != 0o600
        {
            return Err(RegistryError::UnsafePath);
        }
        let database =
            fs::symlink_metadata(&self.database_path).map_err(|_| RegistryError::UnsafePath)?;
        validate_metadata_file(&database)?;
        if (database.dev(), database.ino(), database.uid()) != self.database_identity {
            return Err(RegistryError::UnsafePath);
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoreRecord> {
    let parse_u = |i| -> rusqlite::Result<u64> {
        r.get::<_, String>(i)?
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)
    };
    let state_s: String = r.get(5)?;
    let prior_s: Option<String> = r.get(6)?;
    Ok(StoreRecord {
        store_id: r.get(0)?,
        parent_dev: parse_u(1)?,
        parent_ino: parse_u(2)?,
        root_dev: parse_u(3)?,
        root_ino: parse_u(4)?,
        state: StoreState::parse(&state_s).ok_or(rusqlite::Error::InvalidQuery)?,
        prior_state: prior_s
            .map(|s| StoreState::parse(&s).ok_or(rusqlite::Error::InvalidQuery))
            .transpose()?,
        reserved_bytes: r
            .get::<_, String>(7)?
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_id(id: &str) -> Result<(), RegistryError> {
    if id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(RegistryError::InvalidInput)
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_constraint(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(c,_) if c.code == rusqlite::ErrorCode::ConstraintViolation)
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn initialize_and_classify(c: &mut Connection) -> Result<(), RegistryError> {
    let tx = c
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| RegistryError::Storage)?;
    let app: i64 = tx
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(|_| RegistryError::Storage)?;
    let version: i64 = tx
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| RegistryError::Storage)?;
    let object_count: i64 = tx
        .query_row(
            "SELECT count(*) FROM (SELECT 1 FROM sqlite_master LIMIT 3)",
            [],
            |r| r.get(0),
        )
        .map_err(|_| RegistryError::Storage)?;
    if app == 0 && version == 0 && object_count == 0 {
        tx.execute_batch(TABLE_SCHEMA)
            .map_err(|_| RegistryError::Storage)?;
        tx.execute_batch(&format!(
            "PRAGMA application_id={APP_ID}; PRAGMA user_version={SCHEMA_VERSION};"
        ))
        .map_err(|_| RegistryError::Storage)?;
    }
    if app == APP_ID && version == 1 {
        validate_schema_version(&tx, 1)?;
        validate_records_v1(&tx)?;
        tx.execute_batch("ALTER TABLE verification_stores RENAME TO verification_stores_v1;")
            .map_err(|_| RegistryError::Storage)?;
        tx.execute_batch(TABLE_SCHEMA)
            .map_err(|_| RegistryError::Storage)?;
        tx.execute_batch("INSERT INTO verification_stores(store_id,parent_dev,parent_ino,root_dev,root_ino,state,prior_state,reserved_bytes) SELECT store_id,parent_dev,parent_ino,root_dev,root_ino,state,prior_state,'18446744073709551615' FROM verification_stores_v1; DROP TABLE verification_stores_v1; PRAGMA user_version=2;").map_err(|_| RegistryError::Storage)?;
    }
    validate_schema(&tx)?;
    validate_records(&tx)?;
    tx.execute("UPDATE verification_stores SET prior_state = CASE WHEN state = 'needs_reconciliation' THEN prior_state ELSE state END, state = 'needs_reconciliation' WHERE state != 'cleanup_complete'", []).map_err(|_| RegistryError::Storage)?;
    tx.commit().map_err(|_| RegistryError::Storage)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_schema(c: &Connection) -> Result<(), RegistryError> {
    validate_schema_version(c, 2)
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_schema_version(c: &Connection, version_expected: i64) -> Result<(), RegistryError> {
    let app: i64 = c
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(|_| RegistryError::Storage)?;
    let version: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| RegistryError::Storage)?;
    if app != APP_ID || version != version_expected {
        return Err(RegistryError::UnsupportedSchema);
    }
    let mut statement = c
        .prepare("SELECT CASE WHEN length(CAST(type AS BLOB))<=16 THEN type ELSE '' END,CASE WHEN length(CAST(name AS BLOB))<=128 THEN name ELSE '' END,CASE WHEN length(CAST(tbl_name AS BLOB))<=128 THEN tbl_name ELSE '' END,CASE WHEN sql IS NULL THEN NULL WHEN length(CAST(sql AS BLOB))=?1 THEN sql ELSE '' END FROM sqlite_master LIMIT 3")
        .map_err(|_| RegistryError::Storage)?;
    let objects = statement
        .query_map(
            [if version_expected == 1 {
                V1_TABLE_SCHEMA.len() as i64
            } else {
                TABLE_SCHEMA.len() as i64
            }],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .map_err(|_| RegistryError::Storage)?;
    let objects = objects
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RegistryError::Storage)?;
    if objects.len() != 2 {
        return Err(RegistryError::UnsupportedSchema);
    }
    let expected_table_schema = if version_expected == 1 {
        V1_TABLE_SCHEMA
    } else {
        TABLE_SCHEMA
    };
    let table_ok = objects.iter().any(|o| {
        o.0 == "table"
            && o.1 == "verification_stores"
            && o.2 == "verification_stores"
            && o.3.as_deref() == Some(expected_table_schema)
    });
    let index_ok = objects.iter().any(|o| {
        o.0 == "index"
            && o.1 == "sqlite_autoindex_verification_stores_1"
            && o.2 == "verification_stores"
            && o.3.is_none()
    });
    if !table_ok || !index_ok {
        return Err(RegistryError::UnsupportedSchema);
    }
    Ok(())
}

/// Check record bounds in SQLite before fetching any record values into Rust.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_records(c: &Connection) -> Result<(), RegistryError> {
    let count: i64 = c
        .query_row(
            "SELECT count(*) FROM (SELECT 1 FROM verification_stores LIMIT 4097)",
            [],
            |r| r.get(0),
        )
        .map_err(|_| RegistryError::Storage)?;
    if count > MAX_RECORDS {
        return Err(RegistryError::UnsupportedSchema);
    }
    let malformed: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM verification_stores WHERE
          typeof(store_id)!='text' OR length(CAST(store_id AS BLOB))!=32 OR instr(store_id,char(0))!=0 OR store_id GLOB '*[^0-9a-f]*' OR
          typeof(parent_dev)!='text' OR length(CAST(parent_dev AS BLOB)) NOT BETWEEN 1 AND 20 OR parent_dev GLOB '*[^0-9]*' OR (length(parent_dev)>1 AND substr(parent_dev,1,1)='0') OR
          typeof(parent_ino)!='text' OR length(CAST(parent_ino AS BLOB)) NOT BETWEEN 1 AND 20 OR parent_ino GLOB '*[^0-9]*' OR (length(parent_ino)>1 AND substr(parent_ino,1,1)='0') OR
          typeof(root_dev)!='text' OR length(CAST(root_dev AS BLOB)) NOT BETWEEN 1 AND 20 OR root_dev GLOB '*[^0-9]*' OR (length(root_dev)>1 AND substr(root_dev,1,1)='0') OR
          typeof(root_ino)!='text' OR length(CAST(root_ino AS BLOB)) NOT BETWEEN 1 AND 20 OR root_ino GLOB '*[^0-9]*' OR (length(root_ino)>1 AND substr(root_ino,1,1)='0') OR
          typeof(state)!='text' OR length(CAST(state AS BLOB)) NOT BETWEEN 1 AND 32 OR state NOT IN ('registered','active','stopped','cleanup_pending','cleanup_complete','needs_reconciliation') OR
          (prior_state IS NOT NULL AND (typeof(prior_state)!='text' OR length(CAST(prior_state AS BLOB)) NOT BETWEEN 1 AND 32 OR prior_state NOT IN ('registered','active','stopped','cleanup_pending'))) OR
          (state='needs_reconciliation' AND (prior_state IS NULL OR prior_state='cleanup_complete')) OR
          (state!='needs_reconciliation' AND prior_state IS NOT NULL) OR
          typeof(reserved_bytes)!='text' OR length(CAST(reserved_bytes AS BLOB)) NOT BETWEEN 1 AND 20 OR reserved_bytes GLOB '*[^0-9]*' OR (length(reserved_bytes)>1 AND substr(reserved_bytes,1,1)='0'))",
        [], |r| r.get(0)).map_err(|_| RegistryError::Storage)?;
    if malformed {
        return Err(RegistryError::UnsupportedSchema);
    }
    let mut total = 0u64;
    let mut stmt = c
        .prepare("SELECT reserved_bytes FROM verification_stores")
        .map_err(|_| RegistryError::Storage)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| RegistryError::Storage)?;
    for row in rows {
        let n = row
            .map_err(|_| RegistryError::Storage)?
            .parse::<u64>()
            .map_err(|_| RegistryError::UnsupportedSchema)?;
        if n == u64::MAX {
            continue;
        }
        if n > MAX_STORE_RESERVATION {
            return Err(RegistryError::UnsupportedSchema);
        }
        total = total
            .checked_add(n)
            .ok_or(RegistryError::UnsupportedSchema)?;
    }
    if total > MAX_TOTAL_RESERVATION {
        return Err(RegistryError::UnsupportedSchema);
    }
    let mut stmt = c
        .prepare("SELECT parent_dev,parent_ino,root_dev,root_ino FROM verification_stores")
        .map_err(|_| RegistryError::Storage)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|_| RegistryError::Storage)?;
    for row in rows {
        let (a, b, d, e) = row.map_err(|_| RegistryError::Storage)?;
        if [a, b, d, e].iter().any(|s| s.parse::<u64>().is_err()) {
            return Err(RegistryError::UnsupportedSchema);
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_records_v1(c: &Connection) -> Result<(), RegistryError> {
    validate_records_v1_inner(c)
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_records_v1_inner(c: &Connection) -> Result<(), RegistryError> {
    // The legacy validator is the same bounded identity/lifecycle scan without the new column.
    let count: i64 = c
        .query_row(
            "SELECT count(*) FROM (SELECT 1 FROM verification_stores LIMIT 4097)",
            [],
            |r| r.get(0),
        )
        .map_err(|_| RegistryError::Storage)?;
    if count > MAX_RECORDS {
        return Err(RegistryError::UnsupportedSchema);
    }
    let malformed:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM verification_stores WHERE typeof(store_id)!='text' OR length(CAST(store_id AS BLOB))!=32 OR instr(store_id,char(0))!=0 OR store_id GLOB '*[^0-9a-f]*' OR typeof(parent_dev)!='text' OR length(CAST(parent_dev AS BLOB)) NOT BETWEEN 1 AND 20 OR parent_dev GLOB '*[^0-9]*' OR (length(parent_dev)>1 AND substr(parent_dev,1,1)='0') OR typeof(parent_ino)!='text' OR length(CAST(parent_ino AS BLOB)) NOT BETWEEN 1 AND 20 OR parent_ino GLOB '*[^0-9]*' OR (length(parent_ino)>1 AND substr(parent_ino,1,1)='0') OR typeof(root_dev)!='text' OR length(CAST(root_dev AS BLOB)) NOT BETWEEN 1 AND 20 OR root_dev GLOB '*[^0-9]*' OR (length(root_dev)>1 AND substr(root_dev,1,1)='0') OR typeof(root_ino)!='text' OR length(CAST(root_ino AS BLOB)) NOT BETWEEN 1 AND 20 OR root_ino GLOB '*[^0-9]*' OR (length(root_ino)>1 AND substr(root_ino,1,1)='0') OR typeof(state)!='text' OR state NOT IN ('registered','active','stopped','cleanup_pending','cleanup_complete','needs_reconciliation') OR (prior_state IS NOT NULL AND prior_state NOT IN ('registered','active','stopped','cleanup_pending')) OR (state='needs_reconciliation' AND (prior_state IS NULL OR prior_state='cleanup_complete')) OR (state!='needs_reconciliation' AND prior_state IS NOT NULL))",[],|r|r.get(0)).map_err(|_|RegistryError::Storage)?;
    if malformed {
        return Err(RegistryError::UnsupportedSchema);
    }
    let mut stmt = c
        .prepare("SELECT parent_dev,parent_ino,root_dev,root_ino FROM verification_stores")
        .map_err(|_| RegistryError::Storage)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|_| RegistryError::Storage)?;
    for row in rows {
        let vals = row.map_err(|_| RegistryError::Storage)?;
        if [vals.0, vals.1, vals.2, vals.3]
            .iter()
            .any(|x| x.parse::<u64>().is_err())
        {
            return Err(RegistryError::UnsupportedSchema);
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sibling(path: &Path, suffix: &str) -> Result<PathBuf, RegistryError> {
    let mut name: OsString = path
        .file_name()
        .ok_or(RegistryError::UnsafePath)?
        .to_os_string();
    name.push(suffix);
    Ok(path.with_file_name(name))
}
#[cfg(target_os = "linux")]
fn nofollow_flag() -> i32 {
    rustix::fs::OFlags::NOFOLLOW.bits() as i32
}
#[cfg(target_os = "macos")]
fn nofollow_flag() -> i32 {
    rustix::fs::OFlags::NOFOLLOW.bits() as i32
}
#[cfg(target_os = "linux")]
fn nonblock_flag() -> i32 {
    rustix::fs::OFlags::NONBLOCK.bits() as i32
}
#[cfg(target_os = "macos")]
fn nonblock_flag() -> i32 {
    rustix::fs::OFlags::NONBLOCK.bits() as i32
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_private_dir(p: &Path) -> Result<(), RegistryError> {
    let m = fs::symlink_metadata(p).map_err(|_| RegistryError::UnsafePath)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.mode() & 0o7777 != 0o700
        || m.uid() != geteuid().as_raw()
    {
        return Err(RegistryError::UnsafePath);
    }
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_private_file(f: &File) -> Result<(), RegistryError> {
    let m = f.metadata().map_err(|_| RegistryError::UnsafePath)?;
    if !m.is_file() || m.mode() & 0o7777 != 0o600 || m.nlink() != 1 || m.uid() != geteuid().as_raw()
    {
        return Err(RegistryError::UnsafePath);
    }
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_metadata_file(m: &fs::Metadata) -> Result<(), RegistryError> {
    if !m.is_file() || m.mode() & 0o7777 != 0o600 || m.nlink() != 1 || m.uid() != geteuid().as_raw()
    {
        return Err(RegistryError::UnsafePath);
    }
    Ok(())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_database(p: &Path) -> Result<(), RegistryError> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => Err(RegistryError::UnsafePath),
        Ok(m) => validate_metadata_file(&m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(nofollow_flag())
                .open(p)
                .map_err(|_| RegistryError::UnsafePath)?;
            validate_private_file(&f)
        }
        Err(_) => Err(RegistryError::UnsafePath),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl VerificationStoreRegistry {
    pub fn open(_: impl AsRef<Path>) -> Result<Self, RegistryError> {
        Err(RegistryError::UnsupportedPlatform)
    }
    pub fn reserve(&mut self, _: &str, _: u64, _: u64) -> Result<StoreRecord, RegistryError> {
        Err(RegistryError::UnsupportedPlatform)
    }
    pub fn register(
        &mut self,
        _: &str,
        _: u64,
        _: u64,
        _: u64,
        _: u64,
    ) -> Result<StoreRecord, RegistryError> {
        Err(RegistryError::UnsupportedPlatform)
    }
    pub fn transition(
        &mut self,
        _: &str,
        _: StoreState,
        _: StoreState,
    ) -> Result<StoreRecord, RegistryError> {
        Err(RegistryError::UnsupportedPlatform)
    }
    pub fn get(&self, _: &str) -> Result<Option<StoreRecord>, RegistryError> {
        Err(RegistryError::UnsupportedPlatform)
    }
    pub fn list(&self, _: Option<&str>, _: usize) -> Result<Vec<StoreRecord>, RegistryError> {
        Err(RegistryError::UnsupportedPlatform)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("registry.sqlite");
        (dir, path)
    }

    #[test]
    fn registration_transition_reopen_and_terminal_preservation() {
        let (_dir, path) = setup();
        let mut registry = VerificationStoreRegistry::open(&path).unwrap();
        let id = "0123456789abcdef0123456789abcdef";
        registry
            .register(id, u64::MAX, u64::MAX, u64::MAX, u64::MAX)
            .unwrap();
        assert!(matches!(
            registry.register(id, 4, 5, 6, 7),
            Err(RegistryError::Duplicate)
        ));
        let initial = registry.get(id).unwrap().unwrap();
        assert_eq!(initial.parent_dev, u64::MAX);
        assert_eq!(initial.parent_ino, u64::MAX);
        assert_eq!(initial.root_dev, u64::MAX);
        assert_eq!(initial.root_ino, u64::MAX);
        registry
            .transition(id, StoreState::Registered, StoreState::Active)
            .unwrap();
        registry
            .transition(id, StoreState::Active, StoreState::Stopped)
            .unwrap();
        assert!(matches!(
            registry.transition(id, StoreState::Stopped, StoreState::Active),
            Err(RegistryError::InvalidTransition)
        ));
        assert_eq!(
            registry.get(id).unwrap().unwrap().state,
            StoreState::Stopped
        );
        registry
            .transition(id, StoreState::Stopped, StoreState::CleanupPending)
            .unwrap();
        registry
            .transition(id, StoreState::CleanupPending, StoreState::CleanupComplete)
            .unwrap();
        drop(registry);
        let registry = VerificationStoreRegistry::open(&path).unwrap();
        let record = registry.get(id).unwrap().unwrap();
        assert_eq!(record.state, StoreState::CleanupComplete);
        assert_eq!(record.root_ino, u64::MAX);
    }

    #[test]
    fn reopen_classifies_unfinished_without_losing_prior_state() {
        let (_dir, path) = setup();
        let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut registry = VerificationStoreRegistry::open(&path).unwrap();
        registry.register(id, 1, 2, 3, 4).unwrap();
        registry
            .transition(id, StoreState::Registered, StoreState::Active)
            .unwrap();
        drop(registry);
        let registry = VerificationStoreRegistry::open(&path).unwrap();
        let first = registry.get(id).unwrap().unwrap();
        assert_eq!(first.state, StoreState::NeedsReconciliation);
        assert_eq!(first.prior_state, Some(StoreState::Active));
        assert_eq!(first.parent_ino, 2);
        drop(registry);
        let mut registry = VerificationStoreRegistry::open(&path).unwrap();
        assert_eq!(
            registry.get(id).unwrap().unwrap().prior_state,
            Some(StoreState::Active)
        );
        assert!(matches!(
            registry.transition(id, StoreState::NeedsReconciliation, StoreState::Active),
            Err(RegistryError::InvalidTransition)
        ));
    }

    #[test]
    fn reservations_are_monotonic_bounded_and_survive_reopen() {
        let (_dir, path) = setup();
        let id = "cccccccccccccccccccccccccccccccc";
        let mut r = VerificationStoreRegistry::open(&path).unwrap();
        r.register(id, 1, 2, 3, 4).unwrap();
        assert_eq!(r.reserve(id, 0, 1024).unwrap().reserved_bytes, 1024);
        assert_eq!(r.reserve(id, 1024, 1024).unwrap().reserved_bytes, 1024);
        assert!(matches!(
            r.reserve(id, 0, 1024),
            Err(RegistryError::StaleState)
        ));
        assert!(matches!(
            r.reserve(id, 1024, 0),
            Err(RegistryError::InvalidInput)
        ));
        assert!(matches!(
            r.reserve(id, 1024, MAX_STORE_RESERVATION + 1),
            Err(RegistryError::ReservationLimit)
        ));
        assert_eq!(r.get(id).unwrap().unwrap().reserved_bytes, 1024);
        drop(r);
        let mut r = VerificationStoreRegistry::open(&path).unwrap();
        assert_eq!(r.get(id).unwrap().unwrap().reserved_bytes, 1024);
        assert!(matches!(
            r.reserve(id, 1024, 2048),
            Err(RegistryError::ReconciliationRequired)
        ));
    }

    #[test]
    fn aggregate_reservation_budget_includes_cleanup_complete_rows() {
        let (_dir, path) = setup();
        let a = "dddddddddddddddddddddddddddddddd";
        let b = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let c = "abababababababababababababababab";
        let mut r = VerificationStoreRegistry::open(&path).unwrap();
        for id in [a, b, c] {
            r.register(id, 1, 2, 3, 4).unwrap();
        }
        r.reserve(a, 0, MAX_STORE_RESERVATION).unwrap();
        for (old, new) in [
            (StoreState::Registered, StoreState::Active),
            (StoreState::Active, StoreState::Stopped),
            (StoreState::Stopped, StoreState::CleanupPending),
            (StoreState::CleanupPending, StoreState::CleanupComplete),
        ] {
            r.transition(a, old, new).unwrap();
        }
        r.reserve(b, 0, MAX_STORE_RESERVATION).unwrap();
        assert!(matches!(
            r.reserve(c, 0, 1),
            Err(RegistryError::ReservationLimit)
        ));
        assert_eq!(r.get(c).unwrap().unwrap().reserved_bytes, 0);
        assert_eq!(
            r.get(a).unwrap().unwrap().reserved_bytes,
            MAX_STORE_RESERVATION
        );
    }

    #[test]
    fn v1_migration_preserves_lifecycle_and_marks_unknown_usage() {
        let (_dir, path) = setup();
        let id = "ffffffffffffffffffffffffffffffff";
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(V1_TABLE_SCHEMA).unwrap();
            c.execute("INSERT INTO verification_stores VALUES(?1,'1','2','3','4','cleanup_complete',NULL)",[id]).unwrap();
            c.pragma_update(None, "application_id", APP_ID).unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut r = VerificationStoreRegistry::open(&path).unwrap();
        let rec = r.get(id).unwrap().unwrap();
        assert_eq!(rec.state, StoreState::CleanupComplete);
        assert_eq!(rec.prior_state, None);
        assert_eq!(rec.root_ino, 4);
        assert_eq!(rec.reserved_bytes, u64::MAX);
        let fresh = "abababababababababababababababab";
        r.register(fresh, 1, 2, 3, 4).unwrap();
        assert!(matches!(
            r.reserve(fresh, 0, 1),
            Err(RegistryError::ReconciliationRequired)
        ));
        assert!(matches!(
            r.reserve(id, 0, 1),
            Err(RegistryError::ReconciliationRequired)
        ));
    }

    #[test]
    fn malformed_reservations_refuse_before_mutation() {
        let (_dir, path) = setup();
        let mut r = VerificationStoreRegistry::open(&path).unwrap();
        let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        r.register(id, 1, 2, 3, 4).unwrap();
        r.connection
            .execute_batch("PRAGMA ignore_check_constraints=ON;")
            .unwrap();
        for invalid in ["00", "-1", "18446744073709551616", "2147483649"] {
            r.connection
                .execute(
                    "UPDATE verification_stores SET reserved_bytes=?1",
                    [invalid],
                )
                .unwrap();
            assert!(matches!(
                r.reserve(id, 0, 1),
                Err(RegistryError::UnsupportedSchema)
            ));
            let unchanged: String = r
                .connection
                .query_row(
                    "SELECT reserved_bytes FROM verification_stores",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(unchanged, invalid);
        }
    }

    #[test]
    fn full_v1_registry_migrates_without_losing_identities_or_prior_state() {
        let (_dir, path) = setup();
        {
            let mut c = Connection::open(&path).unwrap();
            c.execute_batch(V1_TABLE_SCHEMA).unwrap();
            c.pragma_update(None, "application_id", APP_ID).unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
            let tx = c.transaction().unwrap();
            for index in 0..MAX_RECORDS {
                tx.execute(
                    "INSERT INTO verification_stores VALUES(?1,?2,'2','3',?3,'active',NULL)",
                    params![
                        format!("{index:032x}"),
                        u64::MAX.to_string(),
                        index.to_string()
                    ],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let r = VerificationStoreRegistry::open(&path).unwrap();
        for index in [0, MAX_RECORDS - 1] {
            let row = r.get(&format!("{index:032x}")).unwrap().unwrap();
            assert_eq!(row.parent_dev, u64::MAX);
            assert_eq!(row.root_ino, index as u64);
            assert_eq!(row.state, StoreState::NeedsReconciliation);
            assert_eq!(row.prior_state, Some(StoreState::Active));
            assert_eq!(row.reserved_bytes, u64::MAX);
        }
        assert_eq!(
            r.connection
                .query_row("SELECT count(*) FROM verification_stores", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            MAX_RECORDS
        );
    }

    #[test]
    fn malformed_v1_is_refused_without_migration_or_repair() {
        let (_dir, path) = setup();
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(V1_TABLE_SCHEMA).unwrap();
            c.pragma_update(None, "application_id", APP_ID).unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
            c.execute_batch("PRAGMA ignore_check_constraints=ON; INSERT INTO verification_stores VALUES('bad','1','2','3','4','active',NULL);").unwrap();
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::UnsupportedSchema)
        ));
        let c = Connection::open(&path).unwrap();
        assert_eq!(
            c.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            c.query_row(
                "SELECT sql FROM sqlite_master WHERE name='verification_stores'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            V1_TABLE_SCHEMA
        );
        assert_eq!(
            c.query_row("SELECT store_id FROM verification_stores", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
            "bad"
        );
    }

    #[test]
    fn rejects_stale_state_and_second_owner() {
        let (_dir, path) = setup();
        let mut first = VerificationStoreRegistry::open(&path).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::LeaseHeld)
        ));
        let id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        first.register(id, 1, 2, 3, 4).unwrap();
        assert!(matches!(
            first.transition(id, StoreState::Active, StoreState::Stopped),
            Err(RegistryError::StaleState)
        ));
    }

    #[test]
    fn rejects_symlinks_and_nonprivate_directory() {
        let (dir, path) = setup();
        let target = dir.path().join("target");
        fs::write(&target, b"x").unwrap();
        symlink(&target, &path).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::UnsafePath)
        ));
        fs::remove_file(&path).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::UnsafePath)
        ));

        let (_dir, path) = setup();
        {
            let _r = VerificationStoreRegistry::open(&path).unwrap();
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::UnsafePath)
        ));

        let (_dir, path) = setup();
        {
            let _r = VerificationStoreRegistry::open(&path).unwrap();
        }
        let lease = sibling(&path, ".lease").unwrap();
        fs::set_permissions(&lease, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::UnsafePath)
        ));
    }

    #[test]
    fn rejects_future_extra_and_tampered_schema() {
        let (_dir, path) = setup();
        {
            let _r = VerificationStoreRegistry::open(&path).unwrap();
        }
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE unexpected(value TEXT)")
            .unwrap();
        drop(c);
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::UnsupportedSchema)
        ));
        assert_eq!(fs::read(&path).unwrap(), before);

        let (_dir, path) = setup();
        {
            let _r = VerificationStoreRegistry::open(&path).unwrap();
        }
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(c);
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::UnsupportedSchema)
        ));
        assert_eq!(fs::read(&path).unwrap(), before);

        let (_dir, path) = setup();
        {
            let _r = VerificationStoreRegistry::open(&path).unwrap();
        }
        let c = Connection::open(&path).unwrap();
        c.execute_batch("DROP TABLE verification_stores; CREATE TABLE verification_stores(store_id TEXT PRIMARY KEY)").unwrap();
        drop(c);
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            VerificationStoreRegistry::open(&path),
            Err(RegistryError::UnsupportedSchema)
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn live_schema_tampering_blocks_every_operation_without_registry_mutation() {
        let (_dir, path) = setup();
        let mut registry = VerificationStoreRegistry::open(&path).unwrap();
        let id = "cccccccccccccccccccccccccccccccc";
        registry.register(id, 1, 2, 3, 4).unwrap();
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TRIGGER unexpected AFTER UPDATE ON verification_stores BEGIN SELECT 1; END",
        )
        .unwrap();
        drop(c);
        assert!(matches!(
            registry.get(id),
            Err(RegistryError::UnsupportedSchema)
        ));
        assert!(matches!(
            registry.list(None, 10),
            Err(RegistryError::UnsupportedSchema)
        ));
        assert!(matches!(
            registry.transition(id, StoreState::Registered, StoreState::Active),
            Err(RegistryError::UnsupportedSchema)
        ));
        assert!(matches!(
            registry.register("dddddddddddddddddddddddddddddddd", 1, 2, 3, 4),
            Err(RegistryError::UnsupportedSchema)
        ));
        let c = Connection::open(&path).unwrap();
        c.execute_batch("DROP TRIGGER unexpected").unwrap();
        drop(c);
        let record = registry.get(id).unwrap().unwrap();
        assert_eq!(record.state, StoreState::Registered);
        assert_eq!(record.prior_state, None);
    }

    #[test]
    fn live_future_version_is_refused_without_registry_write() {
        let (_dir, path) = setup();
        let mut registry = VerificationStoreRegistry::open(&path).unwrap();
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(c);
        let before = fs::read(&path).unwrap();
        assert!(matches!(
            registry.register("abababababababababababababababab", 1, 2, 3, 4),
            Err(RegistryError::UnsupportedSchema)
        ));
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn live_oversized_record_is_rejected_before_dto_read() {
        let (_dir, path) = setup();
        let mut registry = VerificationStoreRegistry::open(&path).unwrap();
        let id = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        registry.register(id, 1, 2, 3, 4).unwrap();
        let nul_id = "11111111111111111111111111111111";
        registry.register(nul_id, 1, 2, 3, 4).unwrap();
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "ignore_check_constraints", "ON")
            .unwrap();
        c.execute(
            "UPDATE verification_stores SET root_ino=?2 WHERE store_id=?1",
            params![id, "9".repeat(10_000)],
        )
        .unwrap();
        let nul_payload = format!("12\0{}", "7".repeat(10_000));
        c.execute(
            "UPDATE verification_stores SET parent_dev=?2 WHERE store_id=?1",
            params![nul_id, nul_payload],
        )
        .unwrap();
        drop(c);
        let injected = "9".repeat(10_000);
        assert!(matches!(
            registry.get(id),
            Err(RegistryError::UnsupportedSchema)
        ));
        assert!(matches!(
            registry.list(None, 2),
            Err(RegistryError::UnsupportedSchema)
        ));
        let c = Connection::open(&path).unwrap();
        let retained: String = c
            .query_row(
                "SELECT root_ino FROM verification_stores WHERE store_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retained, injected);
        c.execute(
            "UPDATE verification_stores SET root_ino='4' WHERE store_id=?1",
            [id],
        )
        .unwrap();
        assert!(matches!(
            registry.get(nul_id),
            Err(RegistryError::UnsupportedSchema)
        ));
        let retained_nul: String = c
            .query_row(
                "SELECT parent_dev FROM verification_stores WHERE store_id=?1",
                [nul_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(retained_nul, format!("12\0{}", "7".repeat(10_000)));
    }

    #[test]
    fn replaced_lease_and_database_paths_are_refused() {
        let (dir, path) = setup();
        let registry = VerificationStoreRegistry::open(&path).unwrap();
        let lease = sibling(&path, ".lease").unwrap();
        let moved_lease = dir.path().join("old.lease");
        fs::rename(&lease, &moved_lease).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lease)
            .unwrap();
        assert!(matches!(
            registry.get("ffffffffffffffffffffffffffffffff"),
            Err(RegistryError::UnsafePath)
        ));
        drop(registry);

        let (_dir, path) = setup();
        let registry = VerificationStoreRegistry::open(&path).unwrap();
        let moved_db = path.with_extension("old");
        fs::rename(&path, &moved_db).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        assert!(matches!(
            registry.get("ffffffffffffffffffffffffffffffff"),
            Err(RegistryError::UnsafePath)
        ));
    }
    #[test]
    fn missing_live_database_is_not_recreated_by_read() {
        let (dir, path) = setup();
        let registry = VerificationStoreRegistry::open(&path).unwrap();
        fs::rename(&path, dir.path().join("retained.sqlite")).unwrap();
        assert!(matches!(
            registry.list(None, 1),
            Err(RegistryError::UnsafePath)
        ));
        assert!(!path.exists());
    }

    #[test]
    fn bounded_pages_and_capacity_preserve_registered_identities() {
        let (_dir, path) = setup();
        let mut registry = VerificationStoreRegistry::open(&path).unwrap();
        let tx = registry.connection.transaction().unwrap();
        for index in 0..MAX_RECORDS {
            tx.execute(
                "INSERT INTO verification_stores VALUES(?1,'1','2','3','4','registered',NULL,'0')",
                [format!("{index:032x}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let first = registry.list(None, 256).unwrap();
        assert_eq!(first.len(), 256);
        let second = registry.list(Some(&first[255].store_id), 256).unwrap();
        assert_eq!(second.len(), 256);
        assert_eq!(second[0].store_id, format!("{:032x}", 256));
        assert!(matches!(
            registry.list(None, 257),
            Err(RegistryError::InvalidInput)
        ));
        assert!(matches!(
            registry.register("ffffffffffffffffffffffffffffffff", 1, 2, 3, 4),
            Err(RegistryError::Capacity)
        ));
        assert_eq!(
            registry
                .get(&format!("{:032x}", 0))
                .unwrap()
                .unwrap()
                .root_ino,
            4
        );
    }

    #[test]
    fn malformed_nul_id_and_missing_prior_state_are_refused_without_repair() {
        for corrupt_id in [false, true] {
            let (_dir, path) = setup();
            let id = "0123456789abcdef0123456789abcdef";
            let mut registry = VerificationStoreRegistry::open(&path).unwrap();
            registry.register(id, 1, 2, 3, 4).unwrap();
            let c = Connection::open(&path).unwrap();
            c.pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
            if corrupt_id {
                c.execute(
                    "UPDATE verification_stores SET store_id=?1",
                    [format!("{}\0", "a".repeat(31))],
                )
                .unwrap();
            } else {
                c.execute(
                    "UPDATE verification_stores SET state='needs_reconciliation',prior_state=NULL",
                    [],
                )
                .unwrap();
            }
            drop(c);
            let before = fs::read(&path).unwrap();
            assert!(matches!(
                registry.list(None, 1),
                Err(RegistryError::UnsupportedSchema)
            ));
            drop(registry);
            assert!(matches!(
                VerificationStoreRegistry::open(&path),
                Err(RegistryError::UnsupportedSchema)
            ));
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }
}
