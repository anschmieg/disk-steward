//! Receipt-addressed purge service for the disposable fixture demo.
//!
//! Batch B of the `fixture-purge-v1` contract: immutable purge reviews, a
//! fail-closed control store, atomic claims, serial execution through
//! `FixtureLifecycle::purge_receipt`, and uncertainty recovery. The service
//! owns no lease: it is constructed by the existing lease-owning coordinator,
//! which supplies the already-opened lifecycle and plan handles. Approval
//! ledger consumption and native confirmation wiring arrive in batch C.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use disk_steward_core::{PlanId, ReceiptId, ReviewId, RevisionId, SessionId, SupportedOperation};
use disk_steward_fixture_lifecycle::{FixtureLifecycle, FixtureStatus, PurgeItemOutcome};
use disk_steward_review_plan::ReviewPlanStore;

const CONTROL_DB: &str = "fixture-purge-control-v1.sqlite3";
const CONTROL_APPLICATION_ID: i64 = 0x4453_4650;
const CONTROL_SCHEMA_VERSION: i64 = 1;
const MAX_TARGETS: usize = 5;
const REVIEW_LIFETIME_MS: i64 = 5 * 60 * 1000;
const MAX_TARGETS_JSON_BYTES: usize = 64 * 1024;

const REVIEWS_SCHEMA: &str = "CREATE TABLE purge_reviews(review_id TEXT PRIMARY KEY, plan_id TEXT NOT NULL, revision_id TEXT NOT NULL, session_id TEXT NOT NULL, operation TEXT NOT NULL CHECK(operation='permanently_remove_staged'), created_at_ms INTEGER NOT NULL, expires_at_ms INTEGER NOT NULL, manifest_sha256 TEXT NOT NULL, targets_json TEXT NOT NULL, selected_logical_bytes INTEGER NOT NULL, state TEXT NOT NULL CHECK(state IN ('prepared','claimed','finished'))) STRICT";
const EXECUTIONS_SCHEMA: &str = "CREATE TABLE purge_executions(execution_id TEXT PRIMARY KEY, review_id TEXT NOT NULL UNIQUE, session_id TEXT NOT NULL, started_at_ms INTEGER NOT NULL, finished_at_ms INTEGER, state TEXT NOT NULL CHECK(state IN ('started','finished','partial','uncertain'))) STRICT";
const CHILD_ATTEMPTS_SCHEMA: &str = "CREATE TABLE purge_child_attempts(execution_id TEXT NOT NULL, attempt_id TEXT NOT NULL PRIMARY KEY, action_id TEXT NOT NULL, receipt_id TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','permanently_removed','uncertain','not_attempted'))) STRICT";
const EXPIRY_INDEX: &str = "CREATE INDEX purge_reviews_expiry ON purge_reviews(expires_at_ms)";

#[derive(Debug)]
pub enum PurgeError {
    StaleRevision,
    ReviewExpired,
    ReviewReplayed,
    ConfirmationDeclined,
    ChangedTarget,
    UnsupportedSchema,
    BusyOwner,
    LedgerUnavailable,
    StorageFailure,
    UncertainOutcome,
    InvalidInput,
}
impl PurgeError {
    /// Sanitized stable error codes; never paths or SQL text.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleRevision => "stale_revision",
            Self::ReviewExpired => "review_expired",
            Self::ReviewReplayed => "review_replayed",
            Self::ConfirmationDeclined => "confirmation_declined",
            Self::ChangedTarget => "changed_target",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::BusyOwner => "busy_owner",
            Self::LedgerUnavailable => "ledger_unavailable",
            Self::StorageFailure => "storage_failure",
            Self::UncertainOutcome => "uncertain_outcome",
            Self::InvalidInput => "invalid_input",
        }
    }
}

impl From<rusqlite::Error> for PurgeError {
    fn from(_: rusqlite::Error) -> Self {
        Self::StorageFailure
    }
}
impl From<std::io::Error> for PurgeError {
    fn from(_: std::io::Error) -> Self {
        Self::StorageFailure
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PurgeTarget {
    pub action_id: disk_steward_core::ActionId,
    pub receipt_id: ReceiptId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurgeReceiptState {
    Staged,
    PurgeIntent,
    PermanentlyRemoved,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PurgeReviewTarget {
    pub action_id: disk_steward_core::ActionId,
    pub receipt_id: ReceiptId,
    pub label: String,
    pub logical_bytes: u64,
    pub status: PurgeReceiptState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PreparedPurgeReview {
    pub schema_version: u32,
    pub kind: String,
    pub review_id: ReviewId,
    pub plan_id: PlanId,
    pub revision_id: RevisionId,
    pub session_id: SessionId,
    pub operation: SupportedOperation,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub manifest_sha256: String,
    pub targets: Vec<PurgeReviewTarget>,
    pub selected_logical_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PurgeItemBatchOutcome {
    pub attempt_id: disk_steward_core::AttemptId,
    pub outcome: PurgeItemOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PurgeExecutionResult {
    pub execution_id: disk_steward_core::ExecutionId,
    pub review_id: ReviewId,
    pub state: String,
    pub items: Vec<PurgeItemBatchOutcome>,
    pub selected_logical_bytes: u64,
    pub durably_unlinked_logical_bytes: u64,
}

/// The private manifest: canonical typed JSON covering the complete frozen
/// review content. Binds every target to its registered content digest and
/// identity digest (which transitively binds device/inode and directory
/// bindings), plus session, plan/revision, operation, versions, byte sum and
/// timestamps. Contains no filesystem paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct PurgeManifestV1 {
    schema_version: u32,
    session_id: SessionId,
    plan_id: PlanId,
    revision_id: RevisionId,
    operation: SupportedOperation,
    created_at_ms: i64,
    expires_at_ms: i64,
    selected_logical_bytes: u64,
    adapter_schema_version: i64,
    targets: Vec<ManifestTargetV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ManifestTargetV1 {
    action_id: disk_steward_core::ActionId,
    receipt_id: ReceiptId,
    logical_bytes: u64,
    content_sha256: String,
    identity_sha256: String,
}

pub struct FixturePurgeService {
    root: PathBuf,
    control: Connection,
    lifecycle: FixtureLifecycle,
    plan: ReviewPlanStore,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl FixturePurgeService {
    /// Open the control store and take ownership of the coordinator's
    /// lifecycle and plan handles. The caller must hold the app-owner lease;
    /// this service never acquires one.
    pub fn open(
        app_owned_root: &Path,
        lifecycle: FixtureLifecycle,
        plan: ReviewPlanStore,
    ) -> Result<Self, PurgeError> {
        Self::open_with_clock(app_owned_root, lifecycle, plan, Box::new(wall_now_ms))
    }

    pub fn open_with_clock(
        app_owned_root: &Path,
        lifecycle: FixtureLifecycle,
        plan: ReviewPlanStore,
        now: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Result<Self, PurgeError> {
        let root = std::fs::canonicalize(app_owned_root)?;
        if !root.is_dir() {
            return Err(PurgeError::StorageFailure);
        }
        let control = open_control_db(&root)?;
        Ok(Self {
            root,
            control,
            lifecycle,
            plan,
            now,
        })
    }
}

fn wall_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn open_control_db(root: &Path) -> Result<Connection, PurgeError> {
    let metadata = std::fs::symlink_metadata(root)?;
    if !metadata.file_type().is_dir() {
        return Err(PurgeError::StorageFailure);
    }
    let db = Connection::open_with_flags(
        root.join(CONTROL_DB),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "FULL")?;
    initialize_control(&db)?;
    Ok(db)
}

fn initialize_control(db: &Connection) -> Result<(), PurgeError> {
    let application_id: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let schema_version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let objects: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
        [],
        |r| r.get(0),
    )?;
    let known: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('purge_reviews','purge_executions','purge_child_attempts')",
        [],
        |r| r.get(0),
    )?;
    let indexes: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='index' AND name='purge_reviews_expiry'",
        [],
        |r| r.get(0),
    )?;
    if objects == 0 {
        // Fresh store: initialize exactly the reviewed schema.
        db.execute_batch(&format!(
            "{REVIEWS_SCHEMA};{EXECUTIONS_SCHEMA};{CHILD_ATTEMPTS_SCHEMA};{EXPIRY_INDEX};"
        ))?;
        db.pragma_update(None, "application_id", CONTROL_APPLICATION_ID)?;
        db.pragma_update(None, "user_version", CONTROL_SCHEMA_VERSION)?;
        return Ok(());
    }
    // Existing store: exact schema, exact objects, known version, no reset.
    if application_id != CONTROL_APPLICATION_ID
        || schema_version != CONTROL_SCHEMA_VERSION
        || objects != 4
        || known != 3
        || indexes != 1
    {
        return Err(PurgeError::UnsupportedSchema);
    }
    let oversized: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' AND (length(CAST(sql AS BLOB))>2048 OR length(CAST(name AS BLOB))>128)",
        [],
        |r| r.get(0),
    )?;
    if oversized != 0 {
        return Err(PurgeError::UnsupportedSchema);
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn demo_root() -> (tempfile::TempDir, [disk_steward_core::ActionId; 5]) {
        (
            tempdir().unwrap(),
            std::array::from_fn(|_| disk_steward_core::ActionId::new()),
        )
    }

    fn open_service(dir: &Path) -> FixturePurgeService {
        let ids = std::array::from_fn(|_| disk_steward_core::ActionId::new());
        let (lifecycle, _) = FixtureLifecycle::create(dir, ids).unwrap();
        let plan = ReviewPlanStore::open(dir.join("review-example-v1.sqlite3")).unwrap();
        FixturePurgeService::open(dir, lifecycle, plan).unwrap()
    }

    #[test]
    fn control_store_initializes_exact_schema_once() {
        let (dir, _ids) = demo_root();
        let _service = open_service(dir.path());
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let application_id: i64 = db
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .unwrap();
        let user_version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(application_id, CONTROL_APPLICATION_ID);
        assert_eq!(user_version, CONTROL_SCHEMA_VERSION);
        let (tables, strict, index): (i64, i64, i64) = db.query_row(
            "SELECT (SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name IN ('purge_reviews','purge_executions','purge_child_attempts')), (SELECT COUNT(*) FROM pragma_table_list WHERE schema='main' AND name IN ('purge_reviews','purge_executions','purge_child_attempts') AND strict=1), (SELECT COUNT(*) FROM sqlite_schema WHERE type='index' AND name='purge_reviews_expiry')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
        assert_eq!((tables, strict, index), (3, 3, 1));
        let journal: String = db
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal.to_lowercase(), "wal");
        let synchronous: i64 = db
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(synchronous, 2, "synchronous FULL");
    }

    #[test]
    fn control_store_reopen_is_idempotent_and_never_resets() {
        let (dir, _ids) = demo_root();
        {
            let _service = open_service(dir.path());
        }
        {
            // Seed a row through a raw connection to prove reopen preserves it.
            let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
            db.execute_batch("INSERT INTO purge_reviews(review_id,plan_id,revision_id,session_id,operation,created_at_ms,expires_at_ms,manifest_sha256,targets_json,selected_logical_bytes,state) VALUES('r','p','v','s','permanently_remove_staged',1,2,'h','[]',0,'prepared')").unwrap();
        }
        {
            let _service = open_service(dir.path());
            let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
            let count: i64 = db
                .query_row("SELECT COUNT(*) FROM purge_reviews", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 1, "reopen must preserve existing rows");
        }
    }

    #[test]
    fn control_store_refuses_unknown_schema_without_reset() {
        let (dir, _ids) = demo_root();
        {
            let _service = open_service(dir.path());
        }
        {
            let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
            db.execute_batch("PRAGMA user_version = 9;").unwrap();
        }
        let (lifecycle, _) = reopen_lifecycle(dir.path());
        let plan = ReviewPlanStore::open(dir.path().join("review-example-v1.sqlite3")).unwrap();
        let error = match FixturePurgeService::open(dir.path(), lifecycle, plan) {
            Err(error) => error,
            Ok(_) => panic!("unknown schema version must be refused"),
        };
        assert_eq!(error.code(), "unsupported_schema");
        // The refused store is not reset.
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let user_version: i64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, 9, "refusal must not rewrite the store");
    }

    #[test]
    fn control_store_refuses_extra_objects_without_reset() {
        let (dir, _ids) = demo_root();
        {
            let _service = open_service(dir.path());
        }
        {
            let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
            db.execute_batch("CREATE TABLE extra(value TEXT NOT NULL);")
                .unwrap();
        }
        let (lifecycle, _) = reopen_lifecycle(dir.path());
        let plan = ReviewPlanStore::open(dir.path().join("review-example-v1.sqlite3")).unwrap();
        let error = match FixturePurgeService::open(dir.path(), lifecycle, plan) {
            Err(error) => error,
            Ok(_) => panic!("extra schema objects must be refused"),
        };
        assert_eq!(error.code(), "unsupported_schema");
    }

    /// Reopen the lifecycle session created inside `dir` by recovering the
    /// session id from the lifecycle manifest.
    fn reopen_lifecycle(
        dir: &Path,
    ) -> (
        FixtureLifecycle,
        disk_steward_fixture_lifecycle::SessionSnapshot,
    ) {
        let entry = std::fs::read_dir(dir)
            .unwrap()
            .find_map(|e| {
                let name = e.unwrap().file_name().into_string().unwrap();
                name.starts_with(".disk-steward-fixtures-").then(|| name)
            })
            .unwrap();
        let db = Connection::open(dir.join(entry).join("lifecycle.sqlite3")).unwrap();
        let wire: String = db
            .query_row(
                "SELECT session_id FROM manifest WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let session: SessionId = serde_json::from_str(&wire).unwrap();
        FixtureLifecycle::reopen(dir, session).unwrap()
    }

    #[test]
    fn purge_error_codes_are_sanitized_stable_strings() {
        assert_eq!(PurgeError::StaleRevision.code(), "stale_revision");
        assert_eq!(PurgeError::ReviewExpired.code(), "review_expired");
        assert_eq!(PurgeError::ReviewReplayed.code(), "review_replayed");
        assert_eq!(
            PurgeError::ConfirmationDeclined.code(),
            "confirmation_declined"
        );
        assert_eq!(PurgeError::ChangedTarget.code(), "changed_target");
        assert_eq!(PurgeError::UnsupportedSchema.code(), "unsupported_schema");
        assert_eq!(PurgeError::BusyOwner.code(), "busy_owner");
        assert_eq!(PurgeError::LedgerUnavailable.code(), "ledger_unavailable");
        assert_eq!(PurgeError::StorageFailure.code(), "storage_failure");
        assert_eq!(PurgeError::UncertainOutcome.code(), "uncertain_outcome");
        assert_eq!(PurgeError::InvalidInput.code(), "invalid_input");
    }

    #[test]
    fn review_dto_wire_shape_is_snake_case_and_frozen() {
        let target = PurgeReviewTarget {
            action_id: disk_steward_core::ActionId::new(),
            receipt_id: ReceiptId::new(),
            label: "demo-fixture".into(),
            logical_bytes: 4096,
            status: PurgeReceiptState::Staged,
        };
        let wire = serde_json::to_string(&target).unwrap();
        assert!(wire.contains("\"receipt_id\""));
        assert!(wire.contains("\"logical_bytes\""));
        assert!(wire.contains("\"staged\""));
        let restored: PurgeReviewTarget = serde_json::from_str(&wire).unwrap();
        assert_eq!(restored, target);
        // The operation serializes with its snake_case variant name.
        let operation_wire =
            serde_json::to_string(&SupportedOperation::PermanentlyRemoveStaged).unwrap();
        assert_eq!(operation_wire, "\"permanently_remove_staged\"");
    }
}
