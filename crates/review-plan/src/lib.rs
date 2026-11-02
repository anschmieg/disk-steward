//! Persistent, review-only synthetic cleanup plan. This crate never accesses
//! user files and its frozen review records are not approvals.
use std::{path::Path, time::Duration};

use disk_steward_core::{ActionId, EvidenceId, PlanId, ReviewId, RevisionId};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const APP_ID: i64 = 0x4453_5250;
const SCHEMA: i64 = 1;
const KIND: &str = "non_executable_example";
const MANIFEST_VERSION: u32 = 1;
const REVIEW_CONTRACT_VERSION: &str = "disk-steward-review-plan-v1";
const MAX_STORED_JSON_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Keep,
    Include,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Build,
    Cache,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceView {
    pub id: EvidenceId,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionView {
    pub id: ActionId,
    pub label: String,
    pub category: Category,
    pub bytes: u64,
    pub decision: Decision,
    pub evidence: Vec<EvidenceView>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanView {
    pub schema_version: u32,
    pub kind: String,
    pub plan_id: PlanId,
    pub revision_id: RevisionId,
    pub revision_number: u64,
    pub actions: Vec<ActionView>,
    pub total_bytes: u64,
    pub selected_bytes: u64,
    pub selected_count: u32,
    pub execution_available: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrozenReview {
    pub schema_version: u32,
    pub kind: String,
    pub review_id: ReviewId,
    pub plan_id: PlanId,
    pub revision_id: RevisionId,
    pub revision_number: u64,
    pub manifest_sha256: String,
    pub actions: Vec<ActionView>,
    pub total_bytes: u64,
    pub selected_bytes: u64,
    pub selected_count: u32,
    pub execution_available: bool,
}

#[derive(Debug)]
pub enum StoreError {
    StaleRevision,
    UnknownAction,
    NothingSelected,
    UnsupportedSchema,
    Storage(rusqlite::Error),
    CorruptData,
    Overflow,
}
impl StoreError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleRevision => "stale_revision",
            Self::UnknownAction => "unknown_action",
            Self::NothingSelected => "nothing_selected",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::Storage(_) => "storage_error",
            Self::CorruptData => "corrupt_data",
            Self::Overflow => "overflow",
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
pub type StoreResult<T> = Result<T, StoreError>;

/// SQLite-backed plan store. `path` is supplied by the trusted native owner.
pub struct ReviewPlanStore {
    connection: Connection,
}
impl ReviewPlanStore {
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let database_path = nofollow_database_path(path.as_ref())?;
        let mut connection = Connection::open_with_flags(
            database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        initialize(&mut connection)?;
        Ok(Self { connection })
    }
    pub fn get_plan(&self) -> StoreResult<PlanView> {
        read_current(&self.connection)
    }
    pub fn edit(
        &mut self,
        expected_revision: RevisionId,
        action: ActionId,
        decision: Decision,
    ) -> StoreResult<PlanView> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let plan = current(&tx)?;
        if plan.revision_id != expected_revision {
            return Err(StoreError::StaleRevision);
        }
        let mut actions = plan.actions.clone();
        let Some(item) = actions.iter_mut().find(|a| a.id == action) else {
            return Err(StoreError::UnknownAction);
        };
        if item.decision == decision {
            return Ok(plan);
        }
        item.decision = decision;
        let number = plan
            .revision_number
            .checked_add(1)
            .ok_or(StoreError::Overflow)?;
        let revision = RevisionId::new();
        let serialized = serde_json::to_string(&actions).map_err(|_| StoreError::CorruptData)?;
        tx.execute(
            "INSERT INTO revisions(revision_id,revision_number,actions_json) VALUES(?1,?2,?3)",
            params![id_string(revision)?, to_i64(number)?, serialized],
        )?;
        tx.execute(
            "UPDATE plan_state SET revision_id=?1 WHERE singleton=1",
            [id_string(revision)?],
        )?;
        tx.commit()?;
        self.get_plan()
    }
    pub fn freeze_review(&mut self, expected_revision: RevisionId) -> StoreResult<FrozenReview> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let plan = current(&tx)?;
        if plan.revision_id != expected_revision {
            return Err(StoreError::StaleRevision);
        }
        if plan.selected_count == 0 {
            return Err(StoreError::NothingSelected);
        }
        if let Some((review_id, digest, json)) = tx
            .query_row(
                "SELECT review_id,manifest_sha256,review_json FROM reviews WHERE revision_id=?1",
                [id_string(plan.revision_id)?],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        {
            let review: FrozenReview = parse_bounded(&json)?;
            verify_review(&review)?;
            verify_review_binding(&review, &plan)?;
            if id_string(review.review_id)? != review_id || review.manifest_sha256 != digest {
                return Err(StoreError::CorruptData);
            }
            return Ok(review);
        }
        let review_id = ReviewId::new();
        validate_actions(&plan.actions)?;
        let hash_input = Manifest {
            review_contract_version: REVIEW_CONTRACT_VERSION,
            manifest_version: MANIFEST_VERSION,
            schema_version: 1,
            kind: KIND,
            plan_id: plan.plan_id,
            revision_id: plan.revision_id,
            revision_number: plan.revision_number,
            actions: &plan.actions,
            total_bytes: plan.total_bytes,
            selected_bytes: plan.selected_bytes,
            selected_count: plan.selected_count,
            execution_available: false,
        };
        let bytes = serde_json::to_vec(&hash_input).map_err(|_| StoreError::CorruptData)?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        let review = FrozenReview {
            schema_version: 1,
            kind: KIND.into(),
            review_id,
            plan_id: plan.plan_id,
            revision_id: plan.revision_id,
            revision_number: plan.revision_number,
            manifest_sha256: digest,
            actions: plan.actions,
            total_bytes: plan.total_bytes,
            selected_bytes: plan.selected_bytes,
            selected_count: plan.selected_count,
            execution_available: false,
        };
        let json = serde_json::to_string(&review).map_err(|_| StoreError::CorruptData)?;
        tx.execute("INSERT INTO reviews(review_id,revision_id,manifest_sha256,review_json) VALUES(?1,?2,?3,?4)", params![id_string(review.review_id)?, id_string(review.revision_id)?, review.manifest_sha256, json])?;
        tx.commit()?;
        Ok(review)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest<'a> {
    review_contract_version: &'static str,
    manifest_version: u32,
    schema_version: u32,
    kind: &'static str,
    plan_id: PlanId,
    revision_id: RevisionId,
    revision_number: u64,
    actions: &'a [ActionView],
    total_bytes: u64,
    selected_bytes: u64,
    selected_count: u32,
    execution_available: bool,
}
fn verify_review(r: &FrozenReview) -> StoreResult<()> {
    if r.schema_version != 1 || r.kind != KIND || r.execution_available {
        return Err(StoreError::CorruptData);
    }
    validate_actions(&r.actions)?;
    let (total, selected, count) = totals(&r.actions)?;
    if (r.total_bytes, r.selected_bytes, r.selected_count) != (total, selected, count) || count == 0
    {
        return Err(StoreError::CorruptData);
    }
    if manifest_digest(r) != r.manifest_sha256 {
        return Err(StoreError::CorruptData);
    }
    Ok(())
}

fn manifest_digest(r: &FrozenReview) -> String {
    let m = Manifest {
        review_contract_version: REVIEW_CONTRACT_VERSION,
        manifest_version: MANIFEST_VERSION,
        schema_version: r.schema_version,
        kind: KIND,
        plan_id: r.plan_id,
        revision_id: r.revision_id,
        revision_number: r.revision_number,
        actions: &r.actions,
        total_bytes: r.total_bytes,
        selected_bytes: r.selected_bytes,
        selected_count: r.selected_count,
        execution_available: false,
    };
    let bytes = serde_json::to_vec(&m).expect("fixed manifest serialization is infallible");
    format!("{:x}", Sha256::digest(bytes))
}
fn parse_bounded<T: for<'de> Deserialize<'de>>(json: &str) -> StoreResult<T> {
    if json.len() > MAX_STORED_JSON_BYTES {
        return Err(StoreError::CorruptData);
    }
    serde_json::from_str(json).map_err(|_| StoreError::CorruptData)
}
fn validate_actions(actions: &[ActionView]) -> StoreResult<()> {
    use std::collections::HashSet;
    if actions.len() != FIXTURES.len() {
        return Err(StoreError::CorruptData);
    }
    let mut action_ids = HashSet::new();
    let mut evidence_ids = HashSet::new();
    for (a, (label, category, bytes)) in actions.iter().zip(FIXTURES) {
        if a.label != label
            || a.category != category
            || a.bytes != bytes
            || a.evidence.len() != 1
            || !action_ids.insert(a.id)
        {
            return Err(StoreError::CorruptData);
        }
        let evidence = &a.evidence[0];
        if evidence.summary != EVIDENCE_SUMMARY || !evidence_ids.insert(evidence.id) {
            return Err(StoreError::CorruptData);
        }
    }
    Ok(())
}
fn verify_review_binding(review: &FrozenReview, plan: &PlanView) -> StoreResult<()> {
    if review.plan_id != plan.plan_id
        || review.revision_id != plan.revision_id
        || review.revision_number != plan.revision_number
        || review.actions != plan.actions
        || review.total_bytes != plan.total_bytes
        || review.selected_bytes != plan.selected_bytes
        || review.selected_count != plan.selected_count
    {
        return Err(StoreError::CorruptData);
    }
    Ok(())
}

fn totals(actions: &[ActionView]) -> StoreResult<(u64, u64, u32)> {
    let mut total = 0u64;
    let mut selected = 0u64;
    let mut count = 0u32;
    for a in actions {
        total = total.checked_add(a.bytes).ok_or(StoreError::Overflow)?;
        if a.decision == Decision::Include {
            selected = selected.checked_add(a.bytes).ok_or(StoreError::Overflow)?;
            count = count.checked_add(1).ok_or(StoreError::Overflow)?;
        }
    }
    Ok((total, selected, count))
}
fn to_i64(v: u64) -> StoreResult<i64> {
    i64::try_from(v).map_err(|_| StoreError::Overflow)
}
fn id_string<T: Serialize>(id: T) -> StoreResult<String> {
    serde_json::to_value(id)
        .map_err(|_| StoreError::CorruptData)?
        .as_str()
        .map(str::to_owned)
        .ok_or(StoreError::CorruptData)
}
fn from_id<T: std::str::FromStr>(s: String) -> StoreResult<T> {
    s.parse().map_err(|_| StoreError::CorruptData)
}
fn current(c: &Connection) -> StoreResult<PlanView> {
    let (plan, revision): (String, String) = c.query_row(
        "SELECT plan_id,revision_id FROM plan_state WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (number, json): (i64, String) = c.query_row(
        "SELECT revision_number,actions_json FROM revisions WHERE revision_id=?1",
        [&revision],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let revision_count: i64 = c.query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get(0))?;
    if number < 1 || revision_count != number {
        return Err(StoreError::CorruptData);
    }
    let actions: Vec<ActionView> = parse_bounded(&json)?;
    validate_actions(&actions)?;
    let (total, selected, count) = totals(&actions)?;
    Ok(PlanView {
        schema_version: 1,
        kind: KIND.into(),
        plan_id: from_id(plan)?,
        revision_id: from_id(revision)?,
        revision_number: u64::try_from(number).map_err(|_| StoreError::CorruptData)?,
        actions,
        total_bytes: total,
        selected_bytes: selected,
        selected_count: count,
        execution_available: false,
    })
}
fn read_current(c: &Connection) -> StoreResult<PlanView> {
    current(c)
}

const STATE:&str="CREATE TABLE plan_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),plan_id TEXT NOT NULL,revision_id TEXT NOT NULL)";
const REVS:&str="CREATE TABLE revisions(revision_id TEXT PRIMARY KEY,revision_number INTEGER NOT NULL UNIQUE,actions_json TEXT NOT NULL)";
const REVIEWS:&str="CREATE TABLE reviews(review_id TEXT PRIMARY KEY,revision_id TEXT NOT NULL UNIQUE,manifest_sha256 TEXT NOT NULL,review_json TEXT NOT NULL,FOREIGN KEY(revision_id) REFERENCES revisions(revision_id))";
fn initialize(c: &mut Connection) -> StoreResult<()> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let c = &tx;
    let app: i64 = c.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let ver: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let tables: i64 = c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
        [],
        |r| r.get(0),
    )?;
    if app == 0 && ver == 0 && tables == 0 {
        tx.execute_batch(&format!("{STATE};{REVS};{REVIEWS};"))?;
        let plan = PlanId::new();
        let revision = RevisionId::new();
        let actions = fixtures();
        let json = serde_json::to_string(&actions).map_err(|_| StoreError::CorruptData)?;
        tx.execute(
            "INSERT INTO revisions VALUES(?1,1,?2)",
            params![id_string(revision)?, json],
        )?;
        tx.execute(
            "INSERT INTO plan_state VALUES(1,?1,?2)",
            params![id_string(plan)?, id_string(revision)?],
        )?;
        tx.execute_batch(&format!(
            "PRAGMA application_id={APP_ID};PRAGMA user_version={SCHEMA};"
        ))?;
        tx.commit()?;
        return Ok(());
    }
    if app != APP_ID || ver != SCHEMA {
        return Err(StoreError::UnsupportedSchema);
    }
    let count: i64 = c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
        [],
        |r| r.get(0),
    )?;
    if count != 3 {
        return Err(StoreError::UnsupportedSchema);
    }
    for (name, sql) in [
        ("plan_state", STATE),
        ("revisions", REVS),
        ("reviews", REVIEWS),
    ] {
        let actual: Option<String> = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        if actual.as_deref().map(normalize) != Some(normalize(sql)) {
            return Err(StoreError::UnsupportedSchema);
        }
    }
    let state: i64 = c.query_row("SELECT COUNT(*) FROM plan_state", [], |r| r.get(0))?;
    if state != 1 {
        return Err(StoreError::UnsupportedSchema);
    }
    let _ = current(c)?;
    tx.commit()?;
    Ok(())
}
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ';')
        .flat_map(char::to_lowercase)
        .collect()
}
const EVIDENCE_SUMMARY: &str = "Disposable synthetic example; not a real detector certification.";
const FIXTURES: [(&str, Category, u64); 5] = [
    ("Compiler output", Category::Build, 3_145_728u64),
    ("Test cache", Category::Cache, 1_048_576),
    ("Preview cache", Category::Cache, 524_288),
    ("Package index", Category::Cache, 262_144),
    ("Incremental output", Category::Build, 131_072),
];
fn fixtures() -> Vec<ActionView> {
    FIXTURES
        .into_iter()
        .map(|(label, category, bytes)| ActionView {
            id: ActionId::new(),
            label: label.into(),
            category,
            bytes,
            decision: Decision::Keep,
            evidence: vec![EvidenceView {
                id: EvidenceId::new(),
                summary: EVIDENCE_SUMMARY.into(),
            }],
        })
        .collect()
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

    fn path() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempdir().unwrap();
        let p = d.path().join("plan.sqlite");
        (d, p)
    }
    #[test]
    fn seeds_exact_fixture_and_keeps_noop_revision_stable() {
        let (_d, p) = path();
        let mut s = ReviewPlanStore::open(&p).unwrap();
        let plan = s.get_plan().unwrap();
        assert_eq!(plan.actions.len(), 5);
        assert_eq!(plan.total_bytes, 5_111_808);
        assert_eq!(plan.selected_count, 0);
        assert!(!plan.execution_available);
        let same = s
            .edit(plan.revision_id, plan.actions[0].id, Decision::Keep)
            .unwrap();
        assert_eq!(same.revision_id, plan.revision_id);
        assert_eq!(same.revision_number, 1);
    }
    #[test]
    fn edit_stale_unknown_and_wrong_revision_are_rejected() {
        let (_d, p) = path();
        let mut s = ReviewPlanStore::open(&p).unwrap();
        let old = s.get_plan().unwrap();
        let changed = s
            .edit(old.revision_id, old.actions[0].id, Decision::Include)
            .unwrap();
        assert_eq!(changed.selected_bytes, 3_145_728);
        assert!(matches!(
            s.edit(old.revision_id, old.actions[1].id, Decision::Include),
            Err(StoreError::StaleRevision)
        ));
        assert!(matches!(
            s.edit(changed.revision_id, ActionId::new(), Decision::Include),
            Err(StoreError::UnknownAction)
        ));
    }
    #[test]
    fn frozen_review_is_immutable_across_edit_and_reopen() {
        let (_d, p) = path();
        let original;
        {
            let mut s = ReviewPlanStore::open(&p).unwrap();
            let plan = s.get_plan().unwrap();
            assert!(matches!(
                s.freeze_review(plan.revision_id),
                Err(StoreError::NothingSelected)
            ));
            let included = s
                .edit(plan.revision_id, plan.actions[0].id, Decision::Include)
                .unwrap();
            original = s.freeze_review(included.revision_id).unwrap();
            assert_eq!(s.freeze_review(included.revision_id).unwrap(), original);
            let edited = s
                .edit(
                    included.revision_id,
                    included.actions[1].id,
                    Decision::Include,
                )
                .unwrap();
            assert_ne!(edited.revision_id, original.revision_id);
        }
        let reopened = ReviewPlanStore::open(&p).unwrap();
        assert_eq!(reopened.get_plan().unwrap().selected_count, 2);
        let conn = Connection::open(&p).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT review_json FROM reviews WHERE revision_id=?1",
                [id_string(original.revision_id).unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<FrozenReview>(&raw).unwrap(),
            original
        );
        verify_review(&original).unwrap();
    }
    #[test]
    fn unknown_schema_is_preserved_and_refused() {
        let (_d, p) = path();
        {
            let c = Connection::open(&p).unwrap();
            c.execute("CREATE TABLE unrelated(value TEXT)", []).unwrap();
            c.execute("INSERT INTO unrelated VALUES('preserve')", [])
                .unwrap();
        }
        assert!(matches!(
            ReviewPlanStore::open(&p),
            Err(StoreError::UnsupportedSchema)
        ));
        let c = Connection::open(&p).unwrap();
        let v: String = c
            .query_row("SELECT value FROM unrelated", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "preserve");
    }
    #[test]
    fn two_connections_compare_and_edit_only_once() {
        let (_d, p) = path();
        let s = ReviewPlanStore::open(&p).unwrap();
        let plan = s.get_plan().unwrap();
        drop(s);
        let barrier = Arc::new(Barrier::new(3));
        let jobs: Vec<_> = (0..2)
            .map(|_| {
                let p = p.clone();
                let b = barrier.clone();
                let rev = plan.revision_id;
                let action = plan.actions[0].id;
                thread::spawn(move || {
                    let mut s = ReviewPlanStore::open(p).unwrap();
                    b.wait();
                    s.edit(rev, action, Decision::Include)
                })
            })
            .collect();
        barrier.wait();
        let out: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
        assert_eq!(out.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            out.iter()
                .filter(|r| matches!(r, Err(StoreError::StaleRevision)))
                .count(),
            1
        );
    }
    #[test]
    fn frozen_hash_uses_struct_order_and_detects_tampering() {
        let (_d, p) = path();
        let mut s = ReviewPlanStore::open(&p).unwrap();
        let plan = s.get_plan().unwrap();
        let included = s
            .edit(plan.revision_id, plan.actions[0].id, Decision::Include)
            .unwrap();
        let review = s.freeze_review(included.revision_id).unwrap();
        verify_review(&review).unwrap();
        let mut altered = review.clone();
        altered.actions[0].bytes += 1;
        assert!(matches!(
            verify_review(&altered),
            Err(StoreError::CorruptData)
        ));
    }
    #[test]
    fn concurrent_first_open_seeds_one_valid_store() {
        let (_d, p) = path();
        let barrier = Arc::new(Barrier::new(5));
        let jobs: Vec<_> = (0..4)
            .map(|_| {
                let p = p.clone();
                let b = barrier.clone();
                thread::spawn(move || {
                    b.wait();
                    ReviewPlanStore::open(p)
                })
            })
            .collect();
        barrier.wait();
        for job in jobs {
            assert!(job.join().unwrap().is_ok());
        }
        let c = Connection::open(&p).unwrap();
        let revisions: i64 = c
            .query_row("SELECT COUNT(*) FROM revisions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(revisions, 1);
    }

    #[test]
    fn self_consistent_review_for_wrong_snapshot_is_rejected() {
        let (_d, p) = path();
        let mut s = ReviewPlanStore::open(&p).unwrap();
        let initial = s.get_plan().unwrap();
        let first = s
            .edit(
                initial.revision_id,
                initial.actions[0].id,
                Decision::Include,
            )
            .unwrap();
        let mut review = s.freeze_review(first.revision_id).unwrap();
        let second = s
            .edit(first.revision_id, first.actions[1].id, Decision::Include)
            .unwrap();
        // Make the old snapshot internally valid and correctly hashed after assigning
        // the new revision token, then attach it to the current revision row.
        review.revision_id = second.revision_id;
        review.revision_number = second.revision_number;
        review.manifest_sha256 = manifest_digest(&review);
        let json = serde_json::to_string(&review).unwrap();
        let c = Connection::open(&p).unwrap();
        c.execute(
            "UPDATE reviews SET revision_id=?1,review_json=?2,manifest_sha256=?3",
            params![
                id_string(second.revision_id).unwrap(),
                json,
                review.manifest_sha256
            ],
        )
        .unwrap();
        drop(c);
        drop(s);
        let mut reopened = ReviewPlanStore::open(&p).unwrap();
        assert!(matches!(
            reopened.freeze_review(second.revision_id),
            Err(StoreError::CorruptData)
        ));
    }
    #[test]
    fn unrelated_view_and_extra_trigger_do_not_pass_schema_validation() {
        let (_dir, p) = path();
        let c = Connection::open(&p).unwrap();
        c.execute_batch("CREATE VIEW sqliteXunrelated AS SELECT 1")
            .unwrap();
        drop(c);
        assert!(matches!(
            ReviewPlanStore::open(&p),
            Err(StoreError::UnsupportedSchema)
        ));
        let c = Connection::open(&p).unwrap();
        let app: i64 = c
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        assert_eq!(app, 0);
        c.execute_batch("DROP VIEW sqliteXunrelated").unwrap();
        drop(c);
        let store = ReviewPlanStore::open(&p).unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER sqliteXunknown_trigger AFTER UPDATE ON plan_state BEGIN SELECT 1; END",
            )
            .unwrap();
        drop(store);
        assert!(matches!(
            ReviewPlanStore::open(&p),
            Err(StoreError::UnsupportedSchema)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn database_symlink_is_refused_without_changing_target() {
        let (dir, target) = path();
        std::fs::write(&target, b"").unwrap();
        let link = dir.path().join("linked.sqlite");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(ReviewPlanStore::open(&link).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"");
    }
}
