//! Durable, single-use bookkeeping for exact approval bindings.
//!
//! This crate does **not** authorize filesystem mutations. Callers must apply
//! their own external policy checks and obtain native user confirmation before
//! consuming an approval or creating an execution intent. It exposes no
//! filesystem cleanup, renderer, shell, or SQL interface.

use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};

const APPLICATION_ID: i64 = 0x4453_414C; // "DSAL"
const SCHEMA_VERSION: i64 = 1;
const APPROVALS_SCHEMA: &str = "CREATE TABLE approvals (
    approval_id TEXT PRIMARY KEY NOT NULL,
    manifest_digest TEXT NOT NULL,
    expires_at_unix_ms INTEGER NOT NULL,
    consumed_at_unix_ms INTEGER,
    CHECK (length(approval_id) BETWEEN 1 AND 512),
    CHECK (length(manifest_digest) BETWEEN 1 AND 512)
)";
const INTENTS_SCHEMA: &str = "CREATE TABLE execution_intents (
    intent_id TEXT PRIMARY KEY NOT NULL,
    approval_id TEXT NOT NULL UNIQUE REFERENCES approvals(approval_id),
    state TEXT NOT NULL CHECK (state IN ('started', 'finished', 'uncertain')),
    started_at_unix_ms INTEGER NOT NULL,
    finished_at_unix_ms INTEGER,
    CHECK ((state = 'started' AND finished_at_unix_ms IS NULL)
        OR (state IN ('finished', 'uncertain') AND finished_at_unix_ms IS NOT NULL))
)";

/// A durable record of an approval ID bound to one exact manifest digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub approval_id: String,
    pub manifest_digest: String,
    pub expires_at_unix_ms: i64,
    pub consumed_at_unix_ms: Option<i64>,
}

/// Durable state for one execution attempt. `Started` left after a restart is
/// intentionally not resumed or executed automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Started,
    Finished,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionIntent {
    pub intent_id: String,
    pub approval_id: String,
    pub state: IntentState,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: Option<i64>,
}

#[derive(Debug)]
pub enum LedgerError {
    Sqlite(rusqlite::Error),
    InvalidInput(&'static str),
    UnsupportedSchema,
    ApprovalNotFound,
    DuplicateApproval,
    DuplicateIntent,
    ApprovalIntentAlreadyExists,
    ManifestMismatch,
    ApprovalExpired,
    ApprovalAlreadyConsumed,
    ApprovalNotConsumed,
    IntentNotFound,
    InvalidIntentTransition,
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "SQLite error: {error}"),
            Self::InvalidInput(reason) => write!(f, "invalid ledger input: {reason}"),
            Self::UnsupportedSchema => write!(
                f,
                "approval ledger database schema is unknown or unsupported"
            ),
            Self::ApprovalNotFound => f.write_str("approval does not exist"),
            Self::DuplicateApproval => f.write_str("approval ID already exists"),
            Self::DuplicateIntent => f.write_str("execution intent ID already exists"),
            Self::ApprovalIntentAlreadyExists => {
                f.write_str("approval already has an execution intent")
            }
            Self::ManifestMismatch => f.write_str("manifest digest does not match the approval"),
            Self::ApprovalExpired => f.write_str("approval is expired"),
            Self::ApprovalAlreadyConsumed => f.write_str("approval was already consumed"),
            Self::ApprovalNotConsumed => {
                f.write_str("approval must be consumed before an execution intent is recorded")
            }
            Self::IntentNotFound => f.write_str("execution intent does not exist"),
            Self::InvalidIntentTransition => f.write_str("execution intent is already terminal"),
        }
    }
}

impl std::error::Error for LedgerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for LedgerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

/// A SQLite-backed ledger. Opening an existing database never resets or
/// migrates an unrecognized schema.
pub struct ApprovalLedger {
    connection: Connection,
}

impl ApprovalLedger {
    /// Open or create the ledger at `path`. A newly created database gets the
    /// current schema; any non-empty database without this ledger's identity
    /// and supported schema version is rejected.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        let database_path = nofollow_database_path(path.as_ref())?;
        let connection = Connection::open_with_flags(
            database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        initialize_or_validate(&connection)?;
        Ok(Self { connection })
    }

    /// Insert an immutable approval binding. Reusing an ID always fails and
    /// never changes its digest, expiry, or consumed state.
    pub fn insert_approval(
        &mut self,
        approval_id: &str,
        manifest_digest: &str,
        expires_at_unix_ms: i64,
    ) -> Result<(), LedgerError> {
        validate_token(approval_id, "approval ID")?;
        validate_token(manifest_digest, "manifest digest")?;
        self.connection
            .execute(
                "INSERT INTO approvals (approval_id, manifest_digest, expires_at_unix_ms, consumed_at_unix_ms)
                 VALUES (?1, ?2, ?3, NULL)",
                params![approval_id, manifest_digest, expires_at_unix_ms],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(ref code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    LedgerError::DuplicateApproval
                }
                other => LedgerError::Sqlite(other),
            })?;
        Ok(())
    }

    /// Atomically consume the exact approval binding once.
    ///
    /// Expiry is exclusive: `now_unix_ms == expires_at_unix_ms` is expired.
    /// The caller must already have passed external policy checks and obtained
    /// native confirmation. A successful result is bookkeeping, not mutation
    /// authority.
    pub fn consume_approval(
        &mut self,
        approval_id: &str,
        manifest_digest: &str,
        now_unix_ms: i64,
    ) -> Result<ApprovalRecord, LedgerError> {
        validate_token(approval_id, "approval ID")?;
        validate_token(manifest_digest, "manifest digest")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record =
            read_approval(&transaction, approval_id)?.ok_or(LedgerError::ApprovalNotFound)?;

        if record.manifest_digest != manifest_digest {
            return Err(LedgerError::ManifestMismatch);
        }
        if record.expires_at_unix_ms <= now_unix_ms {
            return Err(LedgerError::ApprovalExpired);
        }
        if record.consumed_at_unix_ms.is_some() {
            return Err(LedgerError::ApprovalAlreadyConsumed);
        }

        let changed = transaction.execute(
            "UPDATE approvals SET consumed_at_unix_ms = ?2
             WHERE approval_id = ?1 AND manifest_digest = ?3
               AND expires_at_unix_ms > ?2 AND consumed_at_unix_ms IS NULL",
            params![approval_id, now_unix_ms, manifest_digest],
        )?;
        if changed != 1 {
            return Err(LedgerError::ApprovalAlreadyConsumed);
        }
        let consumed = ApprovalRecord {
            consumed_at_unix_ms: Some(now_unix_ms),
            ..record
        };
        transaction.commit()?;
        Ok(consumed)
    }

    /// Persist that a caller began an attempt after consuming its approval.
    /// This does not launch, resume, or authorize an operation.
    pub fn record_intent_started(
        &mut self,
        intent_id: &str,
        approval_id: &str,
        started_at_unix_ms: i64,
    ) -> Result<ExecutionIntent, LedgerError> {
        validate_token(intent_id, "intent ID")?;
        validate_token(approval_id, "approval ID")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_intents WHERE intent_id = ?1)",
            [intent_id],
            |row| row.get(0),
        )?;
        if id_exists {
            return Err(LedgerError::DuplicateIntent);
        }
        let consumed: Option<Option<i64>> = transaction
            .query_row(
                "SELECT consumed_at_unix_ms FROM approvals WHERE approval_id = ?1",
                [approval_id],
                |row| row.get(0),
            )
            .optional()?;
        match consumed {
            None => return Err(LedgerError::ApprovalNotFound),
            Some(None) => return Err(LedgerError::ApprovalNotConsumed),
            Some(Some(_)) => {}
        }
        let intent_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_intents WHERE approval_id = ?1)",
            [approval_id],
            |row| row.get(0),
        )?;
        if intent_exists {
            return Err(LedgerError::ApprovalIntentAlreadyExists);
        }
        transaction
            .execute(
                "INSERT INTO execution_intents (intent_id, approval_id, state, started_at_unix_ms, finished_at_unix_ms)
                 VALUES (?1, ?2, 'started', ?3, NULL)",
                params![intent_id, approval_id, started_at_unix_ms],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(ref code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    LedgerError::DuplicateIntent
                }
                other => LedgerError::Sqlite(other),
            })?;
        transaction.commit()?;
        Ok(ExecutionIntent {
            intent_id: intent_id.to_owned(),
            approval_id: approval_id.to_owned(),
            state: IntentState::Started,
            started_at_unix_ms,
            finished_at_unix_ms: None,
        })
    }

    /// Record a completed attempt. Repeated or conflicting terminal updates
    /// fail, preserving the first durable outcome.
    pub fn record_intent_finished(
        &mut self,
        intent_id: &str,
        finished_at_unix_ms: i64,
    ) -> Result<(), LedgerError> {
        self.finish_intent(intent_id, "finished", finished_at_unix_ms)
    }

    /// Record an attempt whose outcome could not be established. This is a
    /// terminal record and never triggers a retry.
    pub fn record_intent_uncertain(
        &mut self,
        intent_id: &str,
        observed_at_unix_ms: i64,
    ) -> Result<(), LedgerError> {
        self.finish_intent(intent_id, "uncertain", observed_at_unix_ms)
    }

    /// Read persisted attempt state. In particular, a reopened `Started`
    /// record remains only a record; this method never resumes it.
    pub fn execution_intent(&self, intent_id: &str) -> Result<ExecutionIntent, LedgerError> {
        self.connection
            .query_row(
                "SELECT intent_id, approval_id, state, started_at_unix_ms, finished_at_unix_ms
                 FROM execution_intents WHERE intent_id = ?1",
                [intent_id],
                intent_from_row,
            )
            .optional()?
            .ok_or(LedgerError::IntentNotFound)
    }

    fn finish_intent(
        &mut self,
        intent_id: &str,
        state: &str,
        finished_at_unix_ms: i64,
    ) -> Result<(), LedgerError> {
        validate_token(intent_id, "intent ID")?;
        let changed = self.connection.execute(
            "UPDATE execution_intents SET state = ?2, finished_at_unix_ms = ?3
             WHERE intent_id = ?1 AND state = 'started' AND finished_at_unix_ms IS NULL",
            params![intent_id, state, finished_at_unix_ms],
        )?;
        if changed == 1 {
            return Ok(());
        }
        let exists: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM execution_intents WHERE intent_id = ?1)",
            [intent_id],
            |row| row.get(0),
        )?;
        if exists {
            Err(LedgerError::InvalidIntentTransition)
        } else {
            Err(LedgerError::IntentNotFound)
        }
    }
}

fn validate_token(value: &str, label: &'static str) -> Result<(), LedgerError> {
    if value.is_empty() || value.len() > 512 || value.trim() != value || value.contains('\0') {
        return Err(LedgerError::InvalidInput(label));
    }
    Ok(())
}

fn initialize_or_validate(connection: &Connection) -> Result<(), LedgerError> {
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let user_tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
        [],
        |row| row.get(0),
    )?;

    if application_id == 0 && version == 0 && user_tables == 0 {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(&format!("{APPROVALS_SCHEMA}; {INTENTS_SCHEMA};"))?;
        transaction.execute_batch(&format!(
            "PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = {SCHEMA_VERSION};"
        ))?;
        transaction.commit()?;
        return Ok(());
    }

    if application_id != APPLICATION_ID || version != SCHEMA_VERSION {
        return Err(LedgerError::UnsupportedSchema);
    }
    validate_tables(connection)
}

fn validate_tables(connection: &Connection) -> Result<(), LedgerError> {
    let objects: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
        [],
        |row| row.get(0),
    )?;
    if objects != 2 {
        return Err(LedgerError::UnsupportedSchema);
    }

    for (table, expected_sql) in [
        ("approvals", APPROVALS_SCHEMA),
        ("execution_intents", INTENTS_SCHEMA),
    ] {
        let actual_sql: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .optional()?;
        if actual_sql.as_deref().map(normalize_schema_sql)
            != Some(normalize_schema_sql(expected_sql))
        {
            return Err(LedgerError::UnsupportedSchema);
        }
    }
    for (table, columns) in [
        (
            "approvals",
            &[
                ("approval_id", true, true),
                ("manifest_digest", true, false),
                ("expires_at_unix_ms", true, false),
                ("consumed_at_unix_ms", false, false),
            ][..],
        ),
        (
            "execution_intents",
            &[
                ("intent_id", true, true),
                ("approval_id", true, false),
                ("state", true, false),
                ("started_at_unix_ms", true, false),
                ("finished_at_unix_ms", false, false),
            ][..],
        ),
    ] {
        let mut statement = connection.prepare(&format!("PRAGMA table_info('{table}')"))?;
        let found = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, i64>(5)? != 0,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        if found
            .iter()
            .map(|(name, not_null, primary_key)| (name.as_str(), *not_null, *primary_key))
            .collect::<Vec<_>>()
            != columns
        {
            return Err(LedgerError::UnsupportedSchema);
        }
    }
    let foreign_key_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM pragma_foreign_key_list('execution_intents')
         WHERE \"table\" = 'approvals' AND \"from\" = 'approval_id' AND \"to\" = 'approval_id'",
        [],
        |row| row.get(0),
    )?;
    if foreign_key_count != 1 {
        return Err(LedgerError::UnsupportedSchema);
    }
    let unique_approval_index: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_index_list('execution_intents') AS indexes
            WHERE indexes.\"unique\" = 1
              AND (SELECT group_concat(name, ',') FROM pragma_index_info(indexes.name)) = 'approval_id'
         )",
        [],
        |row| row.get(0),
    )?;
    if !unique_approval_index {
        return Err(LedgerError::UnsupportedSchema);
    }
    Ok(())
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

fn read_approval(
    connection: &Connection,
    approval_id: &str,
) -> Result<Option<ApprovalRecord>, LedgerError> {
    Ok(connection
        .query_row(
            "SELECT approval_id, manifest_digest, expires_at_unix_ms, consumed_at_unix_ms
             FROM approvals WHERE approval_id = ?1",
            [approval_id],
            |row| {
                Ok(ApprovalRecord {
                    approval_id: row.get(0)?,
                    manifest_digest: row.get(1)?,
                    expires_at_unix_ms: row.get(2)?,
                    consumed_at_unix_ms: row.get(3)?,
                })
            },
        )
        .optional()?)
}

fn intent_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionIntent> {
    let state: String = row.get(2)?;
    let state = match state.as_str() {
        "started" => IntentState::Started,
        "finished" => IntentState::Finished,
        "uncertain" => IntentState::Uncertain,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(ExecutionIntent {
        intent_id: row.get(0)?,
        approval_id: row.get(1)?,
        state,
        started_at_unix_ms: row.get(3)?,
        finished_at_unix_ms: row.get(4)?,
    })
}

// Resolve trusted parent aliases (notably macOS /var) without resolving the
// final filename. SQLite NOFOLLOW must still reject a symlink at that leaf.
fn nofollow_database_path(path: &Path) -> Result<std::path::PathBuf, rusqlite::Error> {
    let invalid = || rusqlite::Error::InvalidPath(path.to_path_buf());
    let name = path.file_name().ok_or_else(invalid)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent).map_err(|_| invalid())?;
    Ok(parent.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::tempdir;

    fn db_path() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().expect("temporary test directory");
        let path = dir.path().join("approval-ledger.sqlite3");
        (dir, path)
    }

    #[test]
    fn reopening_preserves_consumption_and_started_intents_without_resuming() {
        let (_dir, path) = db_path();
        {
            let mut ledger = ApprovalLedger::open(&path).unwrap();
            ledger
                .insert_approval("approval-1", "sha256:abc", 500)
                .unwrap();
            ledger
                .consume_approval("approval-1", "sha256:abc", 100)
                .unwrap();
            ledger
                .record_intent_started("intent-1", "approval-1", 101)
                .unwrap();
        }
        {
            let mut reopened = ApprovalLedger::open(&path).unwrap();
            assert!(matches!(
                reopened.consume_approval("approval-1", "sha256:abc", 102),
                Err(LedgerError::ApprovalAlreadyConsumed)
            ));
            assert_eq!(
                reopened.execution_intent("intent-1").unwrap().state,
                IntentState::Started
            );
            assert!(matches!(
                reopened.record_intent_started("intent-1", "approval-1", 102),
                Err(LedgerError::DuplicateIntent)
            ));
            assert!(matches!(
                reopened.record_intent_started("intent-2", "approval-1", 102),
                Err(LedgerError::ApprovalIntentAlreadyExists)
            ));
        }
    }

    #[test]
    fn duplicate_insert_cannot_reset_consumed_state() {
        let (_dir, path) = db_path();
        let mut ledger = ApprovalLedger::open(&path).unwrap();
        ledger.insert_approval("a", "digest-1", 1000).unwrap();
        ledger.consume_approval("a", "digest-1", 10).unwrap();
        assert!(matches!(
            ledger.insert_approval("a", "digest-2", 2000),
            Err(LedgerError::DuplicateApproval)
        ));
        assert!(matches!(
            ledger.consume_approval("a", "digest-1", 11),
            Err(LedgerError::ApprovalAlreadyConsumed)
        ));
    }

    #[test]
    fn mismatch_and_expiry_boundary_never_consume() {
        let (_dir, path) = db_path();
        let mut ledger = ApprovalLedger::open(&path).unwrap();
        ledger.insert_approval("valid", "exact", 20).unwrap();
        ledger.insert_approval("boundary", "exact", 20).unwrap();
        assert!(matches!(
            ledger.consume_approval("valid", "other", 10),
            Err(LedgerError::ManifestMismatch)
        ));
        assert!(matches!(
            ledger.consume_approval("boundary", "exact", 20),
            Err(LedgerError::ApprovalExpired)
        ));
        assert!(ledger.consume_approval("valid", "exact", 19).is_ok());
    }

    #[test]
    fn two_connections_can_consume_only_once() {
        let (_dir, path) = db_path();
        let mut setup = ApprovalLedger::open(&path).unwrap();
        setup.insert_approval("contended", "digest", 1000).unwrap();
        drop(setup);

        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let mut ledger = ApprovalLedger::open(path)?;
                    ledger.consume_approval("contended", "digest", 1)
                })
            })
            .collect();
        barrier.wait();
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(LedgerError::ApprovalAlreadyConsumed)))
                .count(),
            1
        );
    }

    #[test]
    fn unknown_database_schema_is_rejected_without_reset() {
        let (_dir, path) = db_path();
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute("CREATE TABLE unrelated (value TEXT NOT NULL)", [])
                .unwrap();
            connection
                .execute("INSERT INTO unrelated VALUES ('preserve-me')", [])
                .unwrap();
        }
        assert!(matches!(
            ApprovalLedger::open(&path),
            Err(LedgerError::UnsupportedSchema)
        ));
        let connection = Connection::open(&path).unwrap();
        let value: String = connection
            .query_row("SELECT value FROM unrelated", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "preserve-me");
    }

    #[test]
    fn intent_terminal_state_is_single_assignment() {
        let (_dir, path) = db_path();
        let mut ledger = ApprovalLedger::open(&path).unwrap();
        ledger.insert_approval("a", "d", 100).unwrap();
        ledger.consume_approval("a", "d", 1).unwrap();
        ledger.record_intent_started("i", "a", 2).unwrap();
        ledger.record_intent_finished("i", 3).unwrap();
        assert_eq!(
            ledger.execution_intent("i").unwrap().state,
            IntentState::Finished
        );
        assert!(matches!(
            ledger.record_intent_uncertain("i", 4),
            Err(LedgerError::InvalidIntentTransition)
        ));
    }
    #[test]
    fn unrelated_view_and_extra_trigger_are_not_an_owned_empty_schema() {
        let (_dir, path) = db_path();
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE VIEW sqliteXunrelated AS SELECT 1")
            .unwrap();
        drop(c);
        assert!(matches!(
            ApprovalLedger::open(&path),
            Err(LedgerError::UnsupportedSchema)
        ));
        let c = Connection::open(&path).unwrap();
        let app: i64 = c
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        assert_eq!(app, 0);
        c.execute_batch("DROP VIEW sqliteXunrelated").unwrap();
        drop(c);
        let ledger = ApprovalLedger::open(&path).unwrap();
        ledger
            .connection
            .execute_batch(
                "CREATE TRIGGER sqliteXunknown_trigger AFTER INSERT ON approvals BEGIN SELECT 1; END",
            )
            .unwrap();
        drop(ledger);
        assert!(matches!(
            ApprovalLedger::open(&path),
            Err(LedgerError::UnsupportedSchema)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn database_symlink_is_refused_without_changing_target() {
        let (dir, target) = db_path();
        std::fs::write(&target, b"").unwrap();
        let link = dir.path().join("linked.sqlite");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(ApprovalLedger::open(&link).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"");
    }
}
