//! Durable SQLite persistence for the non-executable general plan editor.
//!
//! This crate stores only validated plan-model archives and immutable review DTOs.
//! It has no filesystem authority beyond opening its caller-supplied database path.
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use disk_steward_core::{PlanId, ReviewId, RevisionId};
use disk_steward_plan_model::{
    synthetic_fixture, DuplicateSet, EditError, EditOutcome, EditorSnapshot, FrozenReview,
    InventoryItem, PlanEdit, PlanEditor, PlanInput, ValidationError, MAX_SERIALIZED_BYTES,
};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const APP_ID: i64 = 0x4453_5053; // DSPS
const SCHEMA_VERSION: i64 = 1;
const MAX_REVIEWS: i64 = 16;
const MAX_SAFE_JS_LOGICAL_BYTES: u64 = 9_007_199_254_740_991;
const OPEN_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const REVIEW_CONTRACT: &str = "disk-steward-general-review-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureScale {
    Pilot1000,
    Stress10000,
    EmptyReview,
}
impl FixtureScale {
    const fn profile(self) -> &'static str {
        match self {
            Self::Pilot1000 => "pilot_1000",
            Self::Stress10000 => "stress_10000",
            Self::EmptyReview => "live_empty",
        }
    }
    const fn item_count(self) -> usize {
        match self {
            Self::Pilot1000 => 1_000,
            Self::Stress10000 => 10_000,
            Self::EmptyReview => 0,
        }
    }
    const fn group_count(self) -> usize {
        match self {
            Self::Pilot1000 | Self::Stress10000 => 25,
            Self::EmptyReview => 0,
        }
    }
}

#[derive(Debug)]
pub enum StoreError {
    StaleRevision,
    FixtureProfileMismatch,
    UnsupportedSchema,
    CorruptData,
    ReviewNotFound,
    ReviewLimitReached,
    DomainValidation(ValidationError),
    DomainEdit(EditError),
    Storage(rusqlite::Error),
}
impl StoreError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleRevision => "stale_revision",
            Self::FixtureProfileMismatch => "fixture_profile_mismatch",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::CorruptData => "corrupt_data",
            Self::ReviewNotFound => "review_not_found",
            Self::ReviewLimitReached => "review_limit_reached",
            Self::DomainValidation(_) => "invalid_plan",
            Self::DomainEdit(EditError::StaleRevision) => "stale_revision",
            Self::DomainEdit(_) => "invalid_edit",
            Self::Storage(_) => "storage_error",
        }
    }
}
impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for StoreError {}
impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e)
    }
}
impl From<ValidationError> for StoreError {
    fn from(e: ValidationError) -> Self {
        Self::DomainValidation(e)
    }
}
impl From<EditError> for StoreError {
    fn from(e: EditError) -> Self {
        if matches!(e, EditError::StaleRevision) {
            Self::StaleRevision
        } else {
            Self::DomainEdit(e)
        }
    }
}
pub type StoreResult<T> = Result<T, StoreError>;

/// SQLite-backed editor persistence. Each operation reloads the authoritative archive
/// inside a transaction; no in-memory editor cache is retained.
pub struct GeneralPlanStore {
    connection: Connection,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewSummary {
    pub review_id: ReviewId,
    pub revision_id: RevisionId,
    pub revision_number: u64,
    pub selected_count: usize,
    pub logical_selected_bytes: u64,
}

/// Journal-mode conversion can report BUSY without calling SQLite's busy
/// handler when another opener is validating the initialized database. Retry
/// only this idempotent setup pragma, with one shared deadline. Keep it after
/// schema validation; do not change journal mode on an unknown database.
fn enable_wal(connection: &Connection, timeout: Duration) -> rusqlite::Result<()> {
    let deadline = Instant::now() + timeout;
    let result = loop {
        connection.busy_timeout(deadline.saturating_duration_since(Instant::now()))?;
        match connection.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => break Ok(()),
            Err(error) => {
                let busy = matches!(&error, rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::DatabaseBusy);
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !busy || remaining.is_zero() {
                    break Err(error);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
        }
    };
    connection.busy_timeout(OPEN_BUSY_TIMEOUT)?;
    result
}

impl GeneralPlanStore {
    /// Opens a trusted application-owned database path and seeds an empty database once.
    pub fn open(path: impl AsRef<Path>, scale: FixtureScale) -> StoreResult<Self> {
        let path =
            nofollow_database_path(path.as_ref()).map_err(|_| StoreError::UnsupportedSchema)?;
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(OPEN_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        initialize(&mut connection, scale)?;
        enable_wal(&connection, OPEN_BUSY_TIMEOUT)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        Ok(Self { connection })
    }

    pub fn get_plan(&self) -> StoreResult<EditorSnapshot> {
        let tx = self.connection.unchecked_transaction()?;
        let (editor, _) = load_editor(&tx)?;
        let snapshot = editor.snapshot();
        validate_current_review(&tx, &editor)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub fn edit(
        &mut self,
        expected_revision: RevisionId,
        edit: PlanEdit,
    ) -> StoreResult<EditOutcome> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut editor, _) = load_editor(&tx)?;
        validate_all_reviews(&tx, &editor)?;
        check_revision(editor.snapshot().revision_id, expected_revision)?;
        let outcome = editor.edit(expected_revision, edit)?;
        if outcome.changed {
            save_editor(&tx, &editor)?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Appends one inventory record supplied by a trusted native caller. This
    /// accepts no paths, renderer metadata, or execution authority.
    pub fn append_item(
        &mut self,
        expected_revision: RevisionId,
        item: InventoryItem,
    ) -> StoreResult<EditorSnapshot> {
        self.append_items(expected_revision, vec![item])
    }

    /// Atomically appends a bounded batch of trusted inventory records as one
    /// editor operation and one undo step. This accepts no paths or execution
    /// authority.
    pub fn append_items(
        &mut self,
        expected_revision: RevisionId,
        items: Vec<InventoryItem>,
    ) -> StoreResult<EditorSnapshot> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut editor, _) = load_editor(&tx)?;
        validate_all_reviews(&tx, &editor)?;
        check_revision(editor.snapshot().revision_id, expected_revision)?;
        let current = editor.snapshot();
        let existing_total = current
            .items
            .iter()
            .filter_map(|item| item.logical_bytes)
            .try_fold(0u64, u64::checked_add)
            .filter(|total| *total <= MAX_SAFE_JS_LOGICAL_BYTES)
            .ok_or(StoreError::DomainEdit(EditError::InvalidInventory))?;
        items
            .iter()
            .filter_map(|item| item.logical_bytes)
            .try_fold(existing_total, u64::checked_add)
            .filter(|total| *total <= MAX_SAFE_JS_LOGICAL_BYTES)
            .ok_or(StoreError::DomainEdit(EditError::InvalidInventory))?;
        let snapshot = editor.add_items(expected_revision, items)?;
        if snapshot.revision_id != expected_revision {
            save_editor(&tx, &editor)?;
        }
        tx.commit()?;
        Ok(snapshot)
    }

    /// Atomically merges a native read-only duplicate evidence bundle into the plan.
    /// Existing IDs may receive additive evidence; this API carries no paths or execution data.
    pub fn append_duplicate_evidence(
        &mut self,
        expected_revision: RevisionId,
        incoming: Vec<InventoryItem>,
        sets: Vec<DuplicateSet>,
    ) -> StoreResult<EditorSnapshot> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut editor, _) = load_editor(&tx)?;
        validate_all_reviews(&tx, &editor)?;
        check_revision(editor.snapshot().revision_id, expected_revision)?;
        let snapshot = editor.add_duplicate_evidence(expected_revision, incoming, sets)?;
        snapshot
            .items
            .iter()
            .filter_map(|item| item.logical_bytes)
            .try_fold(0u64, u64::checked_add)
            .filter(|total| *total <= MAX_SAFE_JS_LOGICAL_BYTES)
            .ok_or(StoreError::DomainEdit(EditError::InvalidInventory))?;
        if snapshot.revision_id != expected_revision {
            save_editor(&tx, &editor)?;
        }
        tx.commit()?;
        Ok(snapshot)
    }

    pub fn undo(&mut self, expected_revision: RevisionId) -> StoreResult<EditorSnapshot> {
        self.history_operation(expected_revision, true)
    }
    pub fn redo(&mut self, expected_revision: RevisionId) -> StoreResult<EditorSnapshot> {
        self.history_operation(expected_revision, false)
    }
    fn history_operation(
        &mut self,
        expected_revision: RevisionId,
        undo: bool,
    ) -> StoreResult<EditorSnapshot> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (mut editor, _) = load_editor(&tx)?;
        validate_all_reviews(&tx, &editor)?;
        check_revision(editor.snapshot().revision_id, expected_revision)?;
        let snapshot = if undo {
            editor.undo(expected_revision)?
        } else {
            editor.redo(expected_revision)?
        };
        save_editor(&tx, &editor)?;
        tx.commit()?;
        Ok(snapshot)
    }

    pub fn freeze_review(&mut self, expected_revision: RevisionId) -> StoreResult<FrozenReview> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (editor, _) = load_editor(&tx)?;
        validate_all_reviews(&tx, &editor)?;
        check_revision(editor.snapshot().revision_id, expected_revision)?;
        let fresh = editor.freeze_review(expected_revision)?;
        if !fresh.verify_content_hash() {
            return Err(StoreError::CorruptData);
        }
        let revision_id = id_string(fresh.revision_id)?;
        if let Some((stored_id, sha, json)) = tx.query_row(
            "SELECT review_id, review_sha256, review_json FROM frozen_reviews WHERE revision_id=?1 AND length(CAST(review_json AS BLOB))<=?2",
            params![revision_id, MAX_SERIALIZED_BYTES as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        ).optional()? {
            let old: FrozenReview = parse_bounded(&json)?;
            verify_review_row(&old, &stored_id, &sha, &json)?;
            if !same_review_content(&old, &fresh) { return Err(StoreError::CorruptData); }
            tx.commit()?;
            return Ok(old);
        }
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM frozen_reviews", [], |r| r.get(0))?;
        if count >= MAX_REVIEWS {
            return Err(StoreError::ReviewLimitReached);
        }
        insert_review(&tx, &fresh)?;
        tx.commit()?;
        Ok(fresh)
    }

    pub fn list_reviews(&self) -> StoreResult<Vec<ReviewSummary>> {
        let tx = self.connection.unchecked_transaction()?;
        let (editor, _) = load_editor(&tx)?;
        validate_all_reviews(&tx, &editor)?;
        let mut summaries = {
            let mut statement = tx.prepare("SELECT review_json FROM frozen_reviews WHERE length(CAST(review_json AS BLOB))<=?1 LIMIT 17")?;
            let rows = statement
                .query_map([MAX_SERIALIZED_BYTES as i64], |row| row.get::<_, String>(0))?;
            let mut summaries = Vec::new();
            for row in rows {
                let review: FrozenReview = parse_bounded(&row?)?;
                summaries.push(ReviewSummary {
                    review_id: review.review_id,
                    revision_id: review.revision_id,
                    revision_number: review.revision_number,
                    selected_count: review.totals.selected_count,
                    logical_selected_bytes: review.totals.logical_selected_bytes,
                });
            }
            summaries
        };
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.revision_number));
        tx.commit()?;
        Ok(summaries)
    }

    pub fn get_saved_review(&self, review_id: ReviewId) -> StoreResult<FrozenReview> {
        let tx = self.connection.unchecked_transaction()?;
        let (editor, _) = load_editor(&tx)?;
        validate_all_reviews(&tx, &editor)?;
        let key = id_string(review_id)?;
        let Some((stored_id, revision_id, sha, json)) = tx.query_row(
            "SELECT review_id, revision_id, review_sha256, review_json FROM frozen_reviews WHERE review_id=?1 AND length(CAST(review_json AS BLOB))<=?2",
            params![key, MAX_SERIALIZED_BYTES as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)),
        ).optional()? else { return Err(StoreError::ReviewNotFound); };
        let review: FrozenReview = parse_bounded(&json)?;
        verify_review_row(&review, &stored_id, &sha, &json)?;
        if id_string(review.revision_id)? != revision_id {
            return Err(StoreError::CorruptData);
        }
        let current = editor.snapshot();
        if review.plan_id != current.plan_id || review.revision_number > current.revision_number {
            return Err(StoreError::CorruptData);
        }
        if editor.snapshot().revision_id == review.revision_id
            && !same_review_content(&review, &editor.freeze_review(review.revision_id)?)
        {
            return Err(StoreError::CorruptData);
        }
        tx.commit()?;
        Ok(review)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDocument {
    revision_id: String,
    revision_number: i64,
    document_json: Vec<u8>,
    document_sha256: String,
}

const META: &str = "CREATE TABLE store_metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1),profile TEXT NOT NULL,item_count INTEGER NOT NULL,group_count INTEGER NOT NULL) STRICT";
const DOC: &str = "CREATE TABLE current_document(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision_id TEXT NOT NULL,revision_number INTEGER NOT NULL,document_json BLOB NOT NULL,document_sha256 TEXT NOT NULL) STRICT";
const REVIEWS: &str = "CREATE TABLE frozen_reviews(review_id TEXT PRIMARY KEY,revision_id TEXT NOT NULL UNIQUE,review_json TEXT NOT NULL,review_sha256 TEXT NOT NULL) STRICT";

fn initialize(c: &mut Connection, scale: FixtureScale) -> StoreResult<()> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let app: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let objects = object_count(&tx)?;
    if app == 0 && version == 0 && objects == 0 {
        tx.execute_batch(&format!("{META};{DOC};{REVIEWS};"))?;
        let input = match scale {
            FixtureScale::EmptyReview => PlanInput {
                plan_id: PlanId::new(),
                items: Vec::new(),
                groups: Vec::new(),
                duplicate_sets: Vec::new(),
            },
            FixtureScale::Pilot1000 => synthetic_fixture(scale.item_count(), scale.group_count())?,
            FixtureScale::Stress10000 => {
                synthetic_fixture(scale.item_count(), scale.group_count())?
            }
        };
        let editor = PlanEditor::new(input)?;
        let snapshot = editor.snapshot();
        let archive = editor.export_document_json()?;
        tx.execute(
            "INSERT INTO store_metadata VALUES(1,?1,?2,?3)",
            params![
                scale.profile(),
                scale.item_count() as i64,
                scale.group_count() as i64
            ],
        )?;
        let archive_hash = sha256(&archive);
        tx.execute(
            "INSERT INTO current_document VALUES(1,?1,?2,?3,?4)",
            params![
                id_string(snapshot.revision_id)?,
                snapshot.revision_number as i64,
                archive,
                archive_hash
            ],
        )?;
        tx.execute_batch(&format!(
            "PRAGMA application_id={APP_ID};PRAGMA user_version={SCHEMA_VERSION};"
        ))?;
        tx.commit()?;
        return Ok(());
    }
    validate_schema(&tx)?;
    let (profile, items, groups): (String, i64, i64) = tx.query_row(
        "SELECT profile,item_count,group_count FROM store_metadata WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if profile != scale.profile()
        || items != scale.item_count() as i64
        || groups != scale.group_count() as i64
    {
        return Err(StoreError::FixtureProfileMismatch);
    }
    let (editor, _) = load_editor(&tx)?;
    validate_all_reviews(&tx, &editor)?;
    tx.commit()?;
    Ok(())
}

fn validate_schema(c: &Connection) -> StoreResult<()> {
    let app: i64 = c.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if app != APP_ID || version != SCHEMA_VERSION || object_count(c)? != 3 {
        return Err(StoreError::UnsupportedSchema);
    }
    for (name, expected) in [
        ("store_metadata", META),
        ("current_document", DOC),
        ("frozen_reviews", REVIEWS),
    ] {
        let actual: Option<String> = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        if actual.as_deref().map(normalize) != Some(normalize(expected)) {
            return Err(StoreError::UnsupportedSchema);
        }
    }
    let rows: i64 = c.query_row(
        "SELECT (SELECT COUNT(*) FROM store_metadata)+(SELECT COUNT(*) FROM current_document)",
        [],
        |r| r.get(0),
    )?;
    if rows != 2 {
        return Err(StoreError::CorruptData);
    }
    Ok(())
}
fn object_count(c: &Connection) -> StoreResult<i64> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
        [],
        |r| r.get(0),
    )?)
}
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ';')
        .flat_map(char::to_lowercase)
        .collect()
}

fn load_editor(c: &Connection) -> StoreResult<(PlanEditor, StoredDocument)> {
    validate_schema(c)?;
    let (profile, expected_items, expected_groups): (String, i64, i64) = c.query_row(
        "SELECT profile,item_count,group_count FROM store_metadata WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let expected_profile = match (profile.as_str(), expected_items, expected_groups) {
        ("pilot_1000", 1_000, 25) => FixtureScale::Pilot1000,
        ("stress_10000", 10_000, 25) => FixtureScale::Stress10000,
        ("live_empty", 0, 0) => FixtureScale::EmptyReview,
        _ => return Err(StoreError::CorruptData),
    };
    let loaded: rusqlite::Result<(String, i64, Vec<u8>, String)> = c.query_row(
        "SELECT revision_id,revision_number,document_json,document_sha256 FROM current_document WHERE singleton=1 AND length(document_json)<=?1",
        [MAX_SERIALIZED_BYTES as i64], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)));
    let (revision_id, revision_number, bytes, digest) = loaded.map_err(|error| {
        if matches!(error, rusqlite::Error::QueryReturnedNoRows) {
            StoreError::CorruptData
        } else {
            StoreError::Storage(error)
        }
    })?;
    if bytes.len() > MAX_SERIALIZED_BYTES || sha256(&bytes) != digest || revision_number < 1 {
        return Err(StoreError::CorruptData);
    }
    let editor = PlanEditor::import_document_json(&bytes).map_err(|_| StoreError::CorruptData)?;
    let snap = editor.snapshot();
    if id_string(snap.revision_id)? != revision_id
        || i64::try_from(snap.revision_number).ok() != Some(revision_number)
        || snap.items.len() < expected_profile.item_count()
        || snap.groups.len() != expected_profile.group_count()
    {
        return Err(StoreError::CorruptData);
    }
    let stored = StoredDocument {
        revision_id,
        revision_number,
        document_json: bytes,
        document_sha256: digest,
    };
    Ok((editor, stored))
}
fn save_editor(tx: &Transaction<'_>, editor: &PlanEditor) -> StoreResult<()> {
    let snapshot = editor.snapshot();
    let bytes = editor.export_document_json()?;
    if bytes.len() > MAX_SERIALIZED_BYTES {
        return Err(StoreError::CorruptData);
    }
    let digest = sha256(&bytes);
    tx.execute("UPDATE current_document SET revision_id=?1,revision_number=?2,document_json=?3,document_sha256=?4 WHERE singleton=1",
        params![id_string(snapshot.revision_id)?, i64::try_from(snapshot.revision_number).map_err(|_| StoreError::CorruptData)?, bytes, digest])?;
    Ok(())
}
fn validate_current_review(tx: &Transaction<'_>, editor: &PlanEditor) -> StoreResult<()> {
    validate_all_reviews(tx, editor)
}
fn validate_all_reviews(c: &Connection, editor: &PlanEditor) -> StoreResult<()> {
    let current = editor.snapshot();
    let count: i64 = c.query_row("SELECT COUNT(*) FROM frozen_reviews", [], |r| r.get(0))?;
    if count > MAX_REVIEWS {
        return Err(StoreError::CorruptData);
    }
    let mut stmt = c.prepare("SELECT review_id,revision_id,review_sha256,review_json FROM frozen_reviews WHERE length(CAST(review_json AS BLOB))<=?1")?;
    let rows = stmt.query_map([MAX_SERIALIZED_BYTES as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut seen = 0i64;
    for row in rows {
        let (id, revision, hash, json) = row?;
        let review: FrozenReview = parse_bounded(&json)?;
        verify_review_row(&review, &id, &hash, &json)?;
        if id_string(review.revision_id)? != revision
            || review.plan_id != current.plan_id
            || review.revision_number > current.revision_number
        {
            return Err(StoreError::CorruptData);
        }
        if review.revision_id == current.revision_id {
            let fresh = editor.freeze_review(review.revision_id)?;
            if !same_review_content(&review, &fresh) {
                return Err(StoreError::CorruptData);
            }
        }
        seen += 1;
    }
    if seen != count {
        return Err(StoreError::CorruptData);
    }
    Ok(())
}
fn insert_review(tx: &Transaction<'_>, review: &FrozenReview) -> StoreResult<()> {
    let json = serde_json::to_string(review).map_err(|_| StoreError::CorruptData)?;
    if json.len() > MAX_SERIALIZED_BYTES {
        return Err(StoreError::CorruptData);
    }
    tx.execute(
        "INSERT INTO frozen_reviews VALUES(?1,?2,?3,?4)",
        params![
            id_string(review.review_id)?,
            id_string(review.revision_id)?,
            json,
            sha256(json.as_bytes())
        ],
    )?;
    Ok(())
}
fn verify_review_row(review: &FrozenReview, id: &str, hash: &str, json: &str) -> StoreResult<()> {
    if id_string(review.review_id)? != id
        || !review.verify_content_hash()
        || sha256(json.as_bytes()) != hash
    {
        return Err(StoreError::CorruptData);
    }
    Ok(())
}
fn same_review_content(a: &FrozenReview, b: &FrozenReview) -> bool {
    a.schema_version == b.schema_version
        && a.kind == b.kind
        && a.execution_available == b.execution_available
        && a.plan_id == b.plan_id
        && a.revision_id == b.revision_id
        && a.revision_number == b.revision_number
        && a.review_version == REVIEW_CONTRACT
        && b.review_version == REVIEW_CONTRACT
        && a.items == b.items
        && a.groups == b.groups
        && a.duplicates == b.duplicates
        && a.totals == b.totals
        && a.content_hash == b.content_hash
}
fn check_revision(actual: RevisionId, expected: RevisionId) -> StoreResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(StoreError::StaleRevision)
    }
}
fn parse_bounded<T: for<'de> Deserialize<'de>>(json: &str) -> StoreResult<T> {
    if json.len() > MAX_SERIALIZED_BYTES {
        return Err(StoreError::CorruptData);
    }
    serde_json::from_str(json).map_err(|_| StoreError::CorruptData)
}
fn id_string<T: Serialize>(id: T) -> StoreResult<String> {
    serde_json::to_value(id)
        .map_err(|_| StoreError::CorruptData)?
        .as_str()
        .map(str::to_owned)
        .ok_or(StoreError::CorruptData)
}
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn nofollow_database_path(path: &Path) -> std::io::Result<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "database filename required",
        )
    })?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(std::fs::canonicalize(parent)?.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use disk_steward_core::{DuplicateGroupId, EvidenceId, InventoryItemId};
    use disk_steward_plan_model::{Blocker, Choice, ItemKind, PlanEdit, Suggestion};
    use std::{
        fs,
        sync::{Arc, Barrier},
        thread,
    };
    use tempfile::tempdir;

    fn db() -> (tempfile::TempDir, PathBuf) {
        let d = tempdir().unwrap();
        let p = d.path().join("plans.sqlite");
        (d, p)
    }
    fn first_item(s: &EditorSnapshot) -> disk_steward_core::InventoryItemId {
        s.items[0].item_id
    }

    fn metadata_only_item(label: &str) -> InventoryItem {
        InventoryItem {
            id: InventoryItemId::new(),
            parent_id: None,
            label: label.to_owned(),
            kind: ItemKind::File,
            logical_bytes: Some(12),
            recovery_lower_bytes: None,
            recovery_upper_bytes: None,
            storage_identity: None,
            blockers: vec![Blocker {
                code: "native_metadata_only".to_owned(),
                reason: "Only selected-item metadata is available".to_owned(),
            }],
            evidence: vec![disk_steward_plan_model::EvidenceRef {
                id: EvidenceId::new(),
                summary: "Native metadata only; contents were not read".to_owned(),
            }],
            suggestion: Suggestion {
                choice: Choice::Keep,
                rationale: "User-selected item; metadata only".to_owned(),
            },
        }
    }

    fn duplicate_evidence_item(mut item: InventoryItem) -> InventoryItem {
        item.recovery_lower_bytes = None;
        item.recovery_upper_bytes = None;
        item.storage_identity = None;
        item.blockers.push(Blocker {
            code: "read_only_comparison".to_owned(),
            reason: "Read-only duplicate comparison evidence".to_owned(),
        });
        item.evidence.push(disk_steward_plan_model::EvidenceRef {
            id: EvidenceId::new(),
            summary: "Native exact-content comparison; no cleanup authority".to_owned(),
        });
        item.suggestion.choice = Choice::Keep;
        item
    }

    #[test]
    fn seeds_persists_edits_history_and_reviews() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        let initial = store.get_plan().unwrap();
        assert_eq!((initial.items.len(), initial.groups.len()), (1000, 25));
        assert!(!initial.execution_available);
        let edited = store
            .edit(
                initial.revision_id,
                PlanEdit::SetChoice {
                    item_id: first_item(&initial),
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        assert!(edited.changed);
        let review = store.freeze_review(edited.snapshot.revision_id).unwrap();
        assert!(review.verify_content_hash());
        let replayed = store.freeze_review(edited.snapshot.revision_id).unwrap();
        assert_eq!(review, replayed);
        let saved = store.get_saved_review(review.review_id).unwrap();
        assert_eq!(saved, review);
        let changed = store
            .edit(
                edited.snapshot.revision_id,
                PlanEdit::SetMembership {
                    item_id: first_item(&initial),
                    in_plan: false,
                },
            )
            .unwrap();
        assert_ne!(changed.snapshot.revision_id, edited.snapshot.revision_id);
        assert_eq!(store.get_saved_review(review.review_id).unwrap(), review);
        let undone = store.undo(changed.snapshot.revision_id).unwrap();
        assert!(undone.can_redo);
        let redone = store.redo(undone.revision_id).unwrap();
        assert_eq!(redone.revision_number, undone.revision_number + 1);
        let latest_review = store.freeze_review(redone.revision_id).unwrap();
        let summaries = store.list_reviews().unwrap();
        assert_eq!(
            summaries
                .iter()
                .map(|row| row.review_id)
                .collect::<Vec<_>>(),
            vec![latest_review.review_id, review.review_id]
        );
        assert_eq!(
            summaries[0].selected_count,
            latest_review.totals.selected_count
        );
        drop(store);
        let reopened = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        assert_eq!(reopened.get_plan().unwrap(), redone);
        assert_eq!(reopened.get_saved_review(review.review_id).unwrap(), review);
        assert_eq!(
            reopened.list_reviews().unwrap()[0].review_id,
            latest_review.review_id
        );
    }

    #[test]
    fn append_item_preserves_edits_reviews_and_survives_undo_reopen_redo() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        let initial = store.get_plan().unwrap();
        let kept_id = initial.items[0].item_id;
        let edited = store
            .edit(
                initial.revision_id,
                PlanEdit::SetChoice {
                    item_id: kept_id,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        let review = store.freeze_review(edited.snapshot.revision_id).unwrap();
        let item = metadata_only_item("selected-installer.pkg");
        let item_id = item.id;
        let appended = store
            .append_item(edited.snapshot.revision_id, item)
            .unwrap();
        assert_eq!(appended.items.len(), initial.items.len() + 1);
        assert!(appended.items.iter().any(|row| row.item_id == item_id));
        assert_eq!(
            appended
                .items
                .iter()
                .find(|row| row.item_id == kept_id)
                .unwrap()
                .user_choice,
            Some(Choice::Keep)
        );
        assert_eq!(store.get_saved_review(review.review_id).unwrap(), review);

        let undone = store.undo(appended.revision_id).unwrap();
        assert_eq!(undone.items.len(), initial.items.len());
        drop(store);
        let mut reopened = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        assert_eq!(reopened.get_plan().unwrap(), undone);
        let redone = reopened.redo(undone.revision_id).unwrap();
        assert!(redone.items.iter().any(|row| row.item_id == item_id));
        assert_eq!(reopened.get_saved_review(review.review_id).unwrap(), review);
    }

    #[test]
    fn duplicate_evidence_import_is_atomic_blocked_persistent_and_keeps_frozen_reviews() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        let initial = store.get_plan().unwrap();
        let first = metadata_only_item("same-a.bin");
        let second = metadata_only_item("same-b.bin");
        let first_id = first.id;
        let second_id = second.id;
        let appended = store
            .append_items(initial.revision_id, vec![first.clone(), second.clone()])
            .unwrap();
        let edited = store
            .edit(
                appended.revision_id,
                PlanEdit::SetChoice {
                    item_id: first_id,
                    choice: Choice::Keep,
                },
            )
            .unwrap();
        let edited = store
            .edit(
                edited.snapshot.revision_id,
                PlanEdit::SetMembership {
                    item_id: second_id,
                    in_plan: false,
                },
            )
            .unwrap();
        let set_id = DuplicateGroupId::new();
        let set = DuplicateSet {
            id: set_id,
            item_ids: vec![first_id, second_id],
            survivor_id: None,
        };
        let evidence = store
            .append_duplicate_evidence(
                edited.snapshot.revision_id,
                vec![
                    duplicate_evidence_item(first.clone()),
                    duplicate_evidence_item(second.clone()),
                ],
                vec![set.clone()],
            )
            .unwrap();
        let selected = store
            .edit(
                evidence.revision_id,
                PlanEdit::SetDuplicateSurvivor {
                    duplicate_set_id: set_id,
                    survivor_id: first_id,
                },
            )
            .unwrap();
        let frozen = store.freeze_review(selected.snapshot.revision_id).unwrap();
        let third = metadata_only_item("same-c.bin");
        let fourth = metadata_only_item("same-d.bin");
        let new_set = DuplicateSet {
            id: DuplicateGroupId::new(),
            item_ids: vec![third.id, fourth.id],
            survivor_id: None,
        };
        let mut combined = vec![
            duplicate_evidence_item(first.clone()),
            duplicate_evidence_item(second.clone()),
            duplicate_evidence_item(third.clone()),
            duplicate_evidence_item(fourth.clone()),
        ];
        // Reuse the first native evidence exactly to exercise idempotent enrichment.
        combined[0] = evidence
            .items
            .iter()
            .find(|row| row.item_id == first_id)
            .map(|row| {
                let mut value = first.clone();
                value.evidence = row.evidence.clone();
                value.blockers = row.blockers.clone();
                value
            })
            .unwrap();
        let combined = store
            .append_duplicate_evidence(
                selected.snapshot.revision_id,
                combined,
                vec![set.clone(), new_set.clone()],
            )
            .unwrap();
        assert!(!combined.execution_available);
        assert_eq!(
            combined
                .items
                .iter()
                .find(|row| row.item_id == first_id)
                .unwrap()
                .user_choice,
            Some(Choice::Keep)
        );
        assert!(
            !combined
                .items
                .iter()
                .find(|row| row.item_id == second_id)
                .unwrap()
                .in_plan
        );
        assert_eq!(
            combined
                .duplicates
                .iter()
                .find(|row| row.id == set_id)
                .unwrap()
                .survivor_id,
            Some(first_id)
        );
        for id in [third.id, fourth.id] {
            let row = combined.items.iter().find(|row| row.item_id == id).unwrap();
            assert_eq!(row.suggestion.choice, Choice::Keep);
            assert!(row
                .blockers
                .iter()
                .any(|blocker| blocker.code == "read_only_comparison"));
        }
        assert_eq!(store.get_saved_review(frozen.review_id).unwrap(), frozen);

        let unchanged = store.get_plan().unwrap();
        assert!(matches!(
            store.append_duplicate_evidence(RevisionId::new(), vec![], vec![]),
            Err(StoreError::StaleRevision)
        ));
        assert_eq!(store.get_plan().unwrap(), unchanged);
        let mut changed_name = duplicate_evidence_item(first);
        changed_name.label = "different-name.bin".to_owned();
        assert!(matches!(
            store.append_duplicate_evidence(
                unchanged.revision_id,
                vec![changed_name, duplicate_evidence_item(second.clone())],
                vec![set.clone()],
            ),
            Err(StoreError::DomainEdit(EditError::InvalidInventory))
        ));
        assert_eq!(store.get_plan().unwrap(), unchanged);

        let undone = store.undo(combined.revision_id).unwrap();
        assert_eq!(undone.items.len(), selected.snapshot.items.len());
        assert_eq!(
            undone
                .duplicates
                .iter()
                .find(|row| row.id == set_id)
                .unwrap()
                .survivor_id,
            Some(first_id)
        );
        drop(store);
        let mut reopened = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        assert_eq!(reopened.get_plan().unwrap(), undone);
        assert_eq!(reopened.get_saved_review(frozen.review_id).unwrap(), frozen);
        let redone = reopened.redo(undone.revision_id).unwrap();
        assert_eq!(redone.items.len(), combined.items.len());
        assert_eq!(
            redone
                .duplicates
                .iter()
                .find(|row| row.id == set_id)
                .unwrap()
                .survivor_id,
            Some(first_id)
        );
        assert_eq!(reopened.get_saved_review(frozen.review_id).unwrap(), frozen);
    }

    #[test]
    fn duplicate_evidence_import_enforces_safe_merged_logical_total_atomically() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        let mut large = metadata_only_item("near-safe-limit.bin");
        large.logical_bytes = Some(MAX_SAFE_JS_LOGICAL_BYTES - 23);
        let initial = store.get_plan().unwrap();
        let base = store.append_item(initial.revision_id, large).unwrap();
        let first = metadata_only_item("equal-a.bin");
        let second = metadata_only_item("equal-b.bin");
        let set = DuplicateSet {
            id: DuplicateGroupId::new(),
            item_ids: vec![first.id, second.id],
            survivor_id: None,
        };
        let before = store.get_plan().unwrap();
        assert!(matches!(
            store.append_duplicate_evidence(
                base.revision_id,
                vec![
                    duplicate_evidence_item(first),
                    duplicate_evidence_item(second)
                ],
                vec![set],
            ),
            Err(StoreError::DomainEdit(EditError::InvalidInventory))
        ));
        assert_eq!(store.get_plan().unwrap(), before);
    }

    #[test]
    fn append_item_rejects_stale_duplicate_and_over_capacity_without_mutation() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        let before = store.get_plan().unwrap();
        let duplicate = metadata_only_item("duplicate");
        let mut duplicate = duplicate;
        duplicate.id = before.items[0].item_id;
        assert!(matches!(
            store.append_item(before.revision_id, duplicate),
            Err(StoreError::DomainEdit(EditError::InvalidInventory))
        ));
        assert_eq!(store.get_plan().unwrap(), before);
        assert!(matches!(
            store.append_item(RevisionId::new(), metadata_only_item("stale")),
            Err(StoreError::StaleRevision)
        ));
        assert_eq!(store.get_plan().unwrap(), before);

        let (_d, stress_path) = db();
        let mut full = GeneralPlanStore::open(&stress_path, FixtureScale::Stress10000).unwrap();
        let full_before = full.get_plan().unwrap();
        assert!(matches!(
            full.append_item(full_before.revision_id, metadata_only_item("over cap")),
            Err(StoreError::DomainEdit(EditError::InvalidInventory))
        ));
        assert_eq!(full.get_plan().unwrap(), full_before);
    }

    #[test]
    fn empty_live_review_seed_reopens_without_resetting_fixture_profiles() {
        let (_d, path) = db();
        let empty = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        let initial = empty.get_plan().unwrap();
        assert!(initial.items.is_empty());
        assert!(initial.groups.is_empty());
        assert!(initial.duplicates.is_empty());
        drop(empty);

        let reopened = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        assert_eq!(reopened.get_plan().unwrap(), initial);
        assert!(matches!(
            GeneralPlanStore::open(&path, FixtureScale::Pilot1000),
            Err(StoreError::FixtureProfileMismatch)
        ));
        assert_eq!(
            GeneralPlanStore::open(&path, FixtureScale::EmptyReview)
                .unwrap()
                .get_plan()
                .unwrap(),
            initial
        );

        let (_d, fixture_path) = db();
        let fixture = GeneralPlanStore::open(&fixture_path, FixtureScale::Pilot1000).unwrap();
        let fixture_before = fixture.get_plan().unwrap();
        drop(fixture);
        assert!(matches!(
            GeneralPlanStore::open(&fixture_path, FixtureScale::EmptyReview),
            Err(StoreError::FixtureProfileMismatch)
        ));
        assert_eq!(
            GeneralPlanStore::open(&fixture_path, FixtureScale::Pilot1000)
                .unwrap()
                .get_plan()
                .unwrap(),
            fixture_before
        );
    }

    #[test]
    fn batch_append_is_atomic_one_undo_step_and_persists_review_history() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        let before = store.get_plan().unwrap();

        let first = metadata_only_item("observed-file-a");
        let second = metadata_only_item("observed-file-b");
        let mut duplicate_batch = vec![first.clone(), metadata_only_item("observed-file-c")];
        duplicate_batch[1].id = first.id;
        assert!(matches!(
            store.append_items(before.revision_id, duplicate_batch),
            Err(StoreError::DomainEdit(EditError::InvalidInventory))
        ));
        assert_eq!(store.get_plan().unwrap(), before);
        assert!(matches!(
            store.append_items(RevisionId::new(), vec![first.clone(), second.clone()]),
            Err(StoreError::StaleRevision)
        ));
        assert_eq!(store.get_plan().unwrap(), before);

        let appended = store
            .append_items(before.revision_id, vec![first.clone(), second.clone()])
            .unwrap();
        assert_eq!(appended.items.len(), 2);
        assert!(appended.items.iter().any(|item| item.item_id == first.id));
        assert!(appended.items.iter().any(|item| item.item_id == second.id));
        assert_eq!(appended.revision_number, before.revision_number + 1);
        let undone = store.undo(appended.revision_id).unwrap();
        assert!(undone.items.is_empty());
        drop(store);

        let mut reopened = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        assert_eq!(reopened.get_plan().unwrap(), undone);
        let redone = reopened.redo(undone.revision_id).unwrap();
        assert_eq!(redone.items, appended.items);
        assert_eq!(redone.totals, appended.totals);
        assert_ne!(redone.revision_id, appended.revision_id);
        assert_eq!(redone.revision_number, appended.revision_number + 2);
        let review = reopened.freeze_review(redone.revision_id).unwrap();
        assert!(review.verify_content_hash());
        drop(reopened);
        let reopened = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        assert_eq!(reopened.get_plan().unwrap(), redone);
        assert_eq!(reopened.get_saved_review(review.review_id).unwrap(), review);
    }

    #[test]
    fn append_batch_enforces_safe_total_across_live_plan_and_keeps_history() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        let empty = store.get_plan().unwrap();
        let mut first = metadata_only_item("near-safe-limit");
        first.logical_bytes = Some(MAX_SAFE_JS_LOGICAL_BYTES - 1);
        let one = store.append_items(empty.revision_id, vec![first]).unwrap();
        let earlier_review = store.freeze_review(one.revision_id).unwrap();

        let mut second = metadata_only_item("last-safe-byte");
        second.logical_bytes = Some(1);
        let at_limit = store.append_items(one.revision_id, vec![second]).unwrap();
        assert_eq!(
            at_limit
                .items
                .iter()
                .filter_map(|item| item.logical_bytes)
                .sum::<u64>(),
            MAX_SAFE_JS_LOGICAL_BYTES
        );
        let mut extra = metadata_only_item("one-byte-over");
        extra.logical_bytes = Some(1);
        assert!(matches!(
            store.append_items(at_limit.revision_id, vec![extra]),
            Err(StoreError::DomainEdit(EditError::InvalidInventory))
        ));
        assert_eq!(store.get_plan().unwrap(), at_limit);
        assert_eq!(
            store.get_saved_review(earlier_review.review_id).unwrap(),
            earlier_review
        );

        let undone = store.undo(at_limit.revision_id).unwrap();
        assert_eq!(undone.items.len(), 1);
        drop(store);
        let mut reopened = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        assert_eq!(reopened.get_plan().unwrap(), undone);
        let redone = reopened.redo(undone.revision_id).unwrap();
        assert_eq!(redone.items, at_limit.items);
        assert_eq!(redone.totals, at_limit.totals);
        assert_ne!(redone.revision_id, at_limit.revision_id);
        assert_eq!(redone.revision_number, at_limit.revision_number + 2);
        assert_eq!(
            reopened.get_saved_review(earlier_review.review_id).unwrap(),
            earlier_review
        );
    }

    #[test]
    fn removed_items_still_reserve_safe_integer_headroom_for_future_restore() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::EmptyReview).unwrap();
        let empty = store.get_plan().unwrap();
        let mut first = metadata_only_item("retained-logical-headroom");
        first.logical_bytes = Some(MAX_SAFE_JS_LOGICAL_BYTES);
        let id = first.id;
        let added = store.append_items(empty.revision_id, vec![first]).unwrap();
        let removed = store
            .edit(
                added.revision_id,
                PlanEdit::SetMembership {
                    item_id: id,
                    in_plan: false,
                },
            )
            .unwrap()
            .snapshot;
        assert_eq!(removed.totals.in_plan_logical_bytes, 0);
        let mut another = metadata_only_item("one-byte");
        another.logical_bytes = Some(1);
        assert!(store
            .append_items(removed.revision_id, vec![another])
            .is_err());
        assert_eq!(store.get_plan().unwrap(), removed);
        let restored = store.undo(removed.revision_id).unwrap();
        assert_eq!(
            restored.totals.in_plan_logical_bytes,
            MAX_SAFE_JS_LOGICAL_BYTES
        );
    }

    #[test]
    fn stale_edits_across_connections_have_one_winner() {
        let (_d, path) = db();
        let first = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        let revision = first.get_plan().unwrap().revision_id;
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|n| {
                let p = path.clone();
                let b = barrier.clone();
                thread::spawn(move || {
                    let mut store = GeneralPlanStore::open(p, FixtureScale::Pilot1000).unwrap();
                    let snapshot = store.get_plan().unwrap();
                    b.wait();
                    store.edit(
                        revision,
                        PlanEdit::SetMembership {
                            item_id: snapshot.items[n].item_id,
                            in_plan: false,
                        },
                    )
                })
            })
            .collect();
        barrier.wait();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, Err(StoreError::StaleRevision)))
                .count(),
            1
        );
    }

    #[test]
    fn wal_conversion_is_bounded_by_a_reader_and_succeeds_after_release() {
        let (_directory, path) = db();
        let reader = Connection::open(&path).unwrap();
        reader
            .execute_batch("CREATE TABLE fixture (value INTEGER); INSERT INTO fixture VALUES (1);")
            .unwrap();
        reader
            .execute_batch("BEGIN; SELECT * FROM fixture;")
            .unwrap();
        let converter = Connection::open(&path).unwrap();
        let started = Instant::now();
        let error = enable_wal(&converter, Duration::from_millis(40)).unwrap_err();
        assert!(matches!(error, rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::DatabaseBusy));
        assert!(started.elapsed() < Duration::from_secs(2));
        let mode: String = converter
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            reader.execute_batch("ROLLBACK").unwrap();
        });
        let conversion = enable_wal(&converter, Duration::from_secs(1));
        release.join().unwrap();
        conversion.unwrap();
        let mode: String = converter
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn concurrent_first_open_seeds_once_and_profile_is_fixed() {
        let (_d, path) = db();
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let p = path.clone();
                let b = barrier.clone();
                thread::spawn(move || {
                    b.wait();
                    GeneralPlanStore::open(p, FixtureScale::Pilot1000).map(|s| s.get_plan())
                })
            })
            .collect();
        barrier.wait();
        for h in handles {
            assert_eq!(h.join().unwrap().unwrap().unwrap().items.len(), 1000);
        }
        assert!(matches!(
            GeneralPlanStore::open(&path, FixtureScale::Stress10000),
            Err(StoreError::FixtureProfileMismatch)
        ));
    }

    #[test]
    fn rejects_bad_archive_hash_unknown_schema_and_symlink_leaf_without_overwriting_target() {
        let (_d, path) = db();
        {
            GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE current_document SET document_sha256='bad'", [])
            .unwrap();
        drop(conn);
        assert!(matches!(
            GeneralPlanStore::open(&path, FixtureScale::Pilot1000),
            Err(StoreError::CorruptData)
        ));
        let other = path.with_file_name("other.sqlite");
        fs::write(&other, b"preserve me").unwrap();
        #[cfg(unix)]
        let link = path.with_file_name("linked.sqlite");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, &link).unwrap();
        #[cfg(unix)]
        {
            assert!(GeneralPlanStore::open(&link, FixtureScale::Pilot1000).is_err());
            assert_eq!(fs::read(&other).unwrap(), b"preserve me");
        }
    }

    #[test]
    fn unknown_view_and_trigger_are_rejected() {
        for (sql, name) in [
            ("CREATE VIEW unexpected AS SELECT 1", "view.sqlite"),
            (
                "CREATE TRIGGER unexpected AFTER INSERT ON store_metadata BEGIN SELECT 1; END",
                "trigger.sqlite",
            ),
        ] {
            let (_d, path) = db();
            {
                GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
            }
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(sql).unwrap();
            drop(conn);
            assert!(
                matches!(
                    GeneralPlanStore::open(&path, FixtureScale::Pilot1000),
                    Err(StoreError::UnsupportedSchema)
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn live_schema_injection_blocks_reads_and_writes_before_archive_changes() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        let snapshot = store.get_plan().unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE VIEW injected_view AS SELECT 1; CREATE TRIGGER injected_trigger AFTER UPDATE ON current_document BEGIN UPDATE current_document SET document_sha256='changed'; END;",
        )
        .unwrap();
        drop(conn);

        assert!(matches!(
            store.get_plan(),
            Err(StoreError::UnsupportedSchema)
        ));
        assert!(matches!(
            store.edit(
                snapshot.revision_id,
                PlanEdit::SetMembership {
                    item_id: snapshot.items[0].item_id,
                    in_plan: false,
                }
            ),
            Err(StoreError::UnsupportedSchema)
        ));
        let conn = Connection::open(&path).unwrap();
        let (document, digest): (Vec<u8>, String) = conn
            .query_row(
                "SELECT document_json,document_sha256 FROM current_document WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(sha256(&document), digest);
    }

    #[test]
    fn self_consistent_foreign_plan_review_is_rejected_on_open_read_and_edit() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        let current = store.get_plan().unwrap();
        let foreign_editor = PlanEditor::new(synthetic_fixture(1_000, 25).unwrap()).unwrap();
        let foreign = foreign_editor
            .freeze_review(foreign_editor.snapshot().revision_id)
            .unwrap();
        assert!(foreign.verify_content_hash());
        assert_ne!(foreign.plan_id, current.plan_id);
        let json = serde_json::to_string(&foreign).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO frozen_reviews(review_id,revision_id,review_json,review_sha256) VALUES(?1,?2,?3,?4)",
            params![
                id_string(foreign.review_id).unwrap(),
                id_string(foreign.revision_id).unwrap(),
                json,
                sha256(serde_json::to_string(&foreign).unwrap().as_bytes()),
            ],
        )
        .unwrap();
        drop(conn);

        assert!(matches!(
            store.get_saved_review(foreign.review_id),
            Err(StoreError::CorruptData)
        ));
        assert!(matches!(
            store.edit(
                current.revision_id,
                PlanEdit::SetMembership {
                    item_id: current.items[0].item_id,
                    in_plan: false,
                }
            ),
            Err(StoreError::CorruptData)
        ));
        drop(store);
        assert!(matches!(
            GeneralPlanStore::open(&path, FixtureScale::Pilot1000),
            Err(StoreError::CorruptData)
        ));
    }

    #[test]
    fn tampered_review_is_rejected_on_reopen() {
        let (_d, path) = db();
        let mut store = GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        let snapshot = store.get_plan().unwrap();
        let review = store.freeze_review(snapshot.revision_id).unwrap();
        drop(store);
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE frozen_reviews SET review_json=replace(review_json,'non_executable_editor','executable_editor')",
            [],
        )
        .unwrap();
        drop(conn);
        assert!(matches!(
            GeneralPlanStore::open(&path, FixtureScale::Pilot1000),
            Err(StoreError::CorruptData)
        ));
        assert!(!review.execution_available);
    }

    #[test]
    fn oversized_blob_is_checked_in_sql_before_loading() {
        let (_d, path) = db();
        {
            GeneralPlanStore::open(&path, FixtureScale::Pilot1000).unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE current_document SET document_json=zeroblob(?1),document_sha256='oversized'",
            [MAX_SERIALIZED_BYTES as i64 + 1],
        )
        .unwrap();
        drop(conn);
        assert!(matches!(
            GeneralPlanStore::open(&path, FixtureScale::Pilot1000),
            Err(StoreError::CorruptData)
        ));
    }
}
