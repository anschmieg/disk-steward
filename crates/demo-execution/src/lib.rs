//! Bounded coordinator for the five disposable files owned by FixtureLifecycle.
//! This crate does not accept paths from a review or renderer and never handles user files.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use disk_steward_approval_ledger::ApprovalLedger;
use disk_steward_core::{
    ActionBinding, ActionId, Approval, ApprovalId, AttemptId, Digest, ExecutionId, PlanSnapshot,
    PlanSnapshotParts, ReceiptId, ReviewId, RevisionId, ScopeAccess, ScopeId, ScopeSnapshot,
    SessionId, SupportedOperation,
};
use disk_steward_fixture_lifecycle::{
    FixtureLifecycle, FixtureSnapshot, FixtureStatus, LifecycleError, SessionSnapshot,
};
use disk_steward_review_plan::{PlanView, ReviewPlanStore};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const APP_ID: i64 = 0x4453_4445; // DSDE
const SCHEMA_VERSION: i64 = 1;
const EXEC_DB: &str = "demo-execution-v1.sqlite3";
const PLAN_DB: &str = "review-example-v1.sqlite3";
const LEDGER_DB: &str = "approval-ledger-v1.sqlite3";
const REVIEW_TTL_MS: i64 = 5 * 60 * 1000;
const MAX_JSON: usize = 64 * 1024;
const MANIFEST_VERSION: u32 = 1;
const ADAPTER_VERSION: &str = "fixture-lifecycle-v1";
const CONTROL_SCHEMA: &str = "CREATE TABLE control(singleton INTEGER PRIMARY KEY CHECK(singleton=1),session_id TEXT,creation_state TEXT NOT NULL CHECK(creation_state IN ('none','started','ready','inspection')),active_execution_id TEXT) STRICT";
const CLOCK_SCHEMA: &str = "CREATE TABLE clock_guard(singleton INTEGER PRIMARY KEY CHECK(singleton=1),last_now_ms INTEGER NOT NULL) STRICT";
const REVIEWS_SCHEMA: &str = "CREATE TABLE prepared_reviews(review_id TEXT PRIMARY KEY,manifest_json TEXT NOT NULL,manifest_sha256 TEXT NOT NULL,expires_at_ms INTEGER NOT NULL,claimed INTEGER NOT NULL CHECK(claimed IN (0,1))) STRICT";
const EXEC_SCHEMA: &str = "CREATE TABLE executions(execution_id TEXT PRIMARY KEY,review_id TEXT NOT NULL UNIQUE,approval_id TEXT NOT NULL,operation TEXT NOT NULL CHECK(operation IN ('stage','restore')),state TEXT NOT NULL CHECK(state IN ('started','finished','partial','uncertain')),batch_json TEXT NOT NULL,started_at_ms INTEGER NOT NULL,finished_at_ms INTEGER) STRICT";
const ATTEMPTS_SCHEMA: &str = "CREATE TABLE child_attempts(attempt_id TEXT PRIMARY KEY,execution_id TEXT NOT NULL,action_id TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','staged','restored','collision','uncertain')),receipt_id TEXT) STRICT";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DemoFixture {
    pub action_id: ActionId,
    pub receipt_id: Option<ReceiptId>,
    pub status: FixtureStatus,
    pub bytes: u64,
    pub sha256: String,
    pub identity_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DemoStatus {
    pub schema_version: u32,
    pub kind: String,
    pub state: String,
    pub session_id: Option<SessionId>,
    pub fixtures: Vec<DemoFixture>,
    pub last_execution: Option<ExecutionSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedItem {
    pub action_id: ActionId,
    pub label: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedDemoReview {
    pub schema_version: u32,
    pub kind: String,
    pub review_id: ReviewId,
    pub plan_id: disk_steward_core::PlanId,
    pub revision_id: RevisionId,
    pub session_id: SessionId,
    pub operation: String,
    pub items: Vec<PreparedItem>,
    pub selected_bytes: u64,
    pub manifest_sha256: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionItem {
    pub action_id: ActionId,
    pub receipt_id: Option<ReceiptId>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionSummary {
    pub execution_id: ExecutionId,
    pub operation: String,
    pub state: String,
    pub items: Vec<ExecutionItem>,
    pub staged_bytes: u64,
    pub restored_bytes: u64,
    pub recovered_bytes: u64,
}

pub type DemoExecutionResult = ExecutionSummary;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemoError {
    Busy,
    Storage,
    UnsupportedSchema,
    CorruptData,
    PlanUnavailable,
    StaleRevision,
    NothingSelected,
    SessionUnavailable,
    NeedsInspection,
    InvalidReview,
    ReviewExpired,
    ReviewReused,
    ClockReversed,
    ConfirmationCancelled,
    FixtureChanged,
    FixtureUnavailable,
    LedgerUnavailable,
    ExecutionUncertain,
}

impl DemoError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Storage => "storage_unavailable",
            Self::UnsupportedSchema => "unsupported_schema",
            Self::CorruptData => "corrupt_data",
            Self::PlanUnavailable => "plan_unavailable",
            Self::StaleRevision => "stale_revision",
            Self::NothingSelected => "nothing_selected",
            Self::SessionUnavailable => "session_unavailable",
            Self::NeedsInspection => "needs_inspection",
            Self::InvalidReview => "invalid_review",
            Self::ReviewExpired => "review_expired",
            Self::ReviewReused => "review_reused",
            Self::ClockReversed => "clock_reversed",
            Self::ConfirmationCancelled => "confirmation_cancelled",
            Self::FixtureChanged => "fixture_changed",
            Self::FixtureUnavailable => "fixture_unavailable",
            Self::LedgerUnavailable => "ledger_unavailable",
            Self::ExecutionUncertain => "execution_uncertain",
        }
    }
}
impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for DemoError {}
pub type DemoResult<T> = Result<T, DemoError>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileManifest {
    contract: String,
    manifest_version: u32,
    adapter_version: String,
    session_id: SessionId,
    scope_id: ScopeId,
    plan_id: disk_steward_core::PlanId,
    revision_id: RevisionId,
    prepared_review_id: ReviewId,
    created_at_ms: i64,
    expires_at_ms: i64,
    frozen_review_id: ReviewId,
    frozen_review_sha256: String,
    operation: String,
    receipt_id: Option<ReceiptId>,
    fixtures: Vec<FixtureSnapshot>,
    selected: Vec<ActionId>,
}

pub struct DemoExecutionService {
    root: PathBuf,
    lease: Option<std::fs::File>,
    poisoned: bool,
    control: Connection,
    plan: ReviewPlanStore,
    ledger: ApprovalLedger,
    purge: disk_steward_fixture_purge::FixturePurgeService,
    lifecycle: Option<FixtureLifecycle>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl DemoExecutionService {
    pub fn open(app_owned_root: &Path) -> DemoResult<Self> {
        Self::open_with_clock(app_owned_root, wall_now_ms)
    }

    fn open_with_clock(
        app_owned_root: &Path,
        now: impl Fn() -> i64 + Send + Sync + Clone + 'static,
    ) -> DemoResult<Self> {
        let root = fs::canonicalize(app_owned_root).map_err(|_| DemoError::Storage)?;
        if !root.is_dir() {
            return Err(DemoError::Storage);
        }
        let lease = acquire_lease(&root)?;
        let control = open_db(&root.join(EXEC_DB)).map_err(map_sql)?;
        initialize_control(&control).map_err(map_sql)?;
        let plan =
            ReviewPlanStore::open(root.join(PLAN_DB)).map_err(|_| DemoError::PlanUnavailable)?;
        let ledger =
            ApprovalLedger::open(root.join(LEDGER_DB)).map_err(|_| DemoError::LedgerUnavailable)?;
        // The purge control store lives in the same app-owned root and is
        // opened by this lease-owning coordinator (contract: never a second
        // independently leased service). Its recovery pass runs here.
        let now_for_purge = now.clone();
        let purge = disk_steward_fixture_purge::FixturePurgeService::open_with_clock(
            &root,
            Box::new(now_for_purge),
        )
        .map_err(map_purge)?;
        let mut service = Self {
            root,
            lease: Some(lease),
            poisoned: false,
            control,
            plan,
            ledger,
            purge,
            lifecycle: None,
            now: Box::new(now),
        };
        service.recover_started_work()?;
        Ok(service)
    }

    pub fn status(&mut self) -> DemoResult<DemoStatus> {
        let state: String = self
            .control
            .query_row(
                "SELECT creation_state FROM control WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        let session_wire: Option<String> = self
            .control
            .query_row(
                "SELECT session_id FROM control WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        let session_id = session_wire
            .map(|v| serde_json::from_str(&v).map_err(|_| DemoError::CorruptData))
            .transpose()?;
        let mut fixtures = Vec::new();
        let purge_uncertain = self.purge.has_uncertainty().map_err(map_purge)?;
        let mut effective_state = if state == "ready" {
            self.ready_session()?;
            let snapshot = self.snapshot()?;
            fixtures = snapshot.fixtures.into_iter().map(to_demo_fixture).collect();
            if fixtures.iter().any(|f| {
                matches!(
                    f.status,
                    FixtureStatus::Changed
                        | FixtureStatus::Uncertain
                        | FixtureStatus::PermanentlyRemoved
                )
            }) || purge_uncertain
            {
                "needs_inspection".to_owned()
            } else {
                "ready".to_owned()
            }
        } else if state == "started" || state == "inspection" {
            "needs_inspection".to_owned()
        } else {
            "not_started".to_owned()
        };
        let last_execution = self.last_execution()?;
        if last_execution
            .as_ref()
            .is_some_and(|execution| execution.state == "uncertain")
        {
            effective_state = "needs_inspection".into();
        }
        Ok(DemoStatus {
            schema_version: 1,
            kind: "disposable_file_demo".into(),
            state: effective_state,
            session_id,
            fixtures,
            last_execution,
        })
    }

    pub fn start_demo(&mut self) -> DemoResult<DemoStatus> {
        self.ensure_active()?;
        let state: String = self
            .control
            .query_row(
                "SELECT creation_state FROM control WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        if state == "ready" {
            return self.status();
        }
        if state != "none" {
            return Err(DemoError::NeedsInspection);
        }
        let plan = self
            .plan
            .get_plan()
            .map_err(|_| DemoError::PlanUnavailable)?;
        let actions: [ActionId; 5] = plan
            .actions
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| DemoError::PlanUnavailable)?;
        let session_id = SessionId::new();
        let wire = serde_json::to_string(&session_id).map_err(|_| DemoError::CorruptData)?;
        let tx = self.control.unchecked_transaction().map_err(map_sql)?;
        let changed = tx.execute("UPDATE control SET session_id=?1,creation_state='started' WHERE singleton=1 AND creation_state='none'", [&wire]).map_err(map_sql)?;
        if changed != 1 {
            return Err(DemoError::NeedsInspection);
        }
        tx.commit().map_err(map_sql)?;
        let (lifecycle, _) = FixtureLifecycle::create_for_session(&self.root, session_id, actions)
            .map_err(|_| DemoError::NeedsInspection)?;
        self.control.execute("UPDATE control SET creation_state='ready' WHERE singleton=1 AND session_id=?1 AND creation_state='started'", [&wire]).map_err(map_sql)?;
        self.lifecycle = Some(lifecycle);
        self.status()
    }

    pub fn prepare_stage(
        &mut self,
        expected_revision: RevisionId,
    ) -> DemoResult<PreparedDemoReview> {
        self.prepare(expected_revision, "stage", None)
    }

    pub fn prepare_restore(
        &mut self,
        expected_revision: RevisionId,
        receipt: ReceiptId,
    ) -> DemoResult<PreparedDemoReview> {
        self.prepare(expected_revision, "restore", Some(receipt))
    }

    pub fn confirm_and_execute(
        &mut self,
        review_id: ReviewId,
        confirmer: impl FnOnce(&PreparedDemoReview) -> bool,
    ) -> DemoResult<DemoExecutionResult> {
        self.ensure_active()?;
        // Cross-store uncertainty consult (contract fixture-purge-v1).
        if self.purge.has_uncertainty().map_err(map_purge)? {
            return Err(DemoError::NeedsInspection);
        }
        let now = self.checked_now()?;
        let (prepared, manifest, digest) = self.read_review(review_id)?;
        if prepared.expires_at_ms <= now {
            return Err(DemoError::ReviewExpired);
        }
        self.revalidate(&prepared, &manifest, &digest, now)?;
        if !confirmer(&prepared) {
            return Err(DemoError::ConfirmationCancelled);
        }
        let after = self.checked_now()?;
        if prepared.expires_at_ms <= after {
            return Err(DemoError::ReviewExpired);
        }
        self.revalidate(&prepared, &manifest, &digest, after)?;

        let current_plan = self
            .plan
            .get_plan()
            .map_err(|_| DemoError::PlanUnavailable)?;
        let snapshot = make_core_snapshot(&manifest, &prepared, &current_plan)?;
        let approval = Approval::issue(&snapshot, after as u64, prepared.expires_at_ms as u64)
            .ok_or(DemoError::InvalidReview)?;
        approval
            .validate(&snapshot, after as u64)
            .map_err(|_| DemoError::InvalidReview)?;
        let approval_id = approval.id();
        let execution_id = ExecutionId::new();
        let attempts: Vec<AttemptId> = prepared.items.iter().map(|_| AttemptId::new()).collect();
        let batch = BatchBinding {
            execution_id,
            approval_id,
            attempts: attempts.clone(),
            review_id,
            digest: digest.clone(),
        };
        let batch_json = bounded_json(&batch)?;
        let digest_bytes = decode_sha256(&manifest_digest_for_core(&manifest, &prepared)?)?;
        let core_manifest = hex(&digest_bytes);
        let approval_wire = wire(&approval_id)?;
        let execution_wire = wire(&execution_id)?;
        let tx = self
            .control
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let claim = tx.execute("UPDATE prepared_reviews SET claimed=1 WHERE review_id=?1 AND manifest_sha256=?2 AND expires_at_ms>?3 AND claimed=0", params![wire(&review_id)?, digest, after]).map_err(map_sql)?;
        if claim != 1 {
            return Err(DemoError::ReviewReused);
        }
        let active = tx.execute("UPDATE control SET active_execution_id=?1 WHERE singleton=1 AND active_execution_id IS NULL", [wire(&execution_id)?]).map_err(map_sql)?;
        if active != 1 {
            return Err(DemoError::ReviewReused);
        }
        tx.execute("INSERT INTO executions(execution_id,review_id,approval_id,operation,state,batch_json,started_at_ms,finished_at_ms) VALUES(?1,?2,?3,?4,'started',?5,?6,NULL)", params![wire(&execution_id)?, wire(&review_id)?, wire(&approval_id)?, prepared.operation, batch_json, after]).map_err(map_sql)?;
        for ((item, attempt), _) in prepared.items.iter().zip(attempts.iter()).zip(0..) {
            tx.execute("INSERT INTO child_attempts(attempt_id,execution_id,action_id,state,receipt_id) VALUES(?1,?2,?3,'pending',NULL)", params![wire(attempt)?, wire(&execution_id)?, wire(&item.action_id)?]).map_err(map_sql)?;
        }
        tx.commit().map_err(map_sql)?;

        let ledger_result = self
            .ledger
            .insert_approval(&approval_wire, &core_manifest, prepared.expires_at_ms)
            .and_then(|()| {
                self.ledger
                    .consume_approval(&approval_wire, &core_manifest, after)
                    .map(|_| ())
            })
            .and_then(|()| {
                self.ledger
                    .record_intent_started(&execution_wire, &approval_wire, after)
                    .map(|_| ())
            });
        if ledger_result.is_err() {
            self.mark_claim_uncertain(&prepared, execution_id, &attempts, after)?;
            return Err(DemoError::LedgerUnavailable);
        }
        let execution_time = self.checked_now();
        let execution_check = match execution_time {
            Ok(time) => self
                .revalidate(&prepared, &manifest, &digest, time)
                .map(|()| time),
            Err(error) => Err(error),
        };
        let execution_time = match execution_check {
            Ok(time) => time,
            Err(error) => {
                self.mark_claim_uncertain(&prepared, execution_id, &attempts, after)?;
                return Err(error);
            }
        };
        match self.execute_batch(&prepared, execution_id, &attempts, execution_time) {
            Ok(result) => Ok(result),
            Err(error) => {
                self.mark_claim_uncertain(&prepared, execution_id, &attempts, (self.now)())?;
                Err(error)
            }
        }
    }

    /// Whether open uncertainty exists in either control store: the demo
    /// execution store's uncertain executions or the purge store's started
    /// executions and pending child attempts.
    fn uncertainty_across_stores(&self) -> DemoResult<bool> {
        let demo_uncertain: i64 = self
            .control
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM executions WHERE state='uncertain')",
                [],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        Ok(demo_uncertain != 0 || self.purge.has_uncertainty().map_err(map_purge)?)
    }

    /// Prepare an immutable purge review over explicitly selected staged
    /// receipts (contract fixture-purge-v1). Consults uncertainty in both
    /// control stores first; the caller selects every target explicitly.
    pub fn prepare_purge_review(
        &mut self,
        expected_revision: RevisionId,
        targets: Vec<disk_steward_fixture_purge::PurgeTarget>,
    ) -> DemoResult<disk_steward_fixture_purge::PreparedPurgeReview> {
        self.ensure_active()?;
        if self.uncertainty_across_stores()? {
            return Err(DemoError::NeedsInspection);
        }
        self.ready_session()?;
        let lifecycle = self.lifecycle.as_mut().ok_or(DemoError::NeedsInspection)?;
        self.purge
            .prepare_purge_review(lifecycle, &self.plan, expected_revision, targets)
            .map_err(map_purge)
    }

    /// Confirm and execute a prepared purge review: fresh revalidation
    /// before and after the trusted confirmation callback, a core approval
    /// reconstructed for the exact frozen plan/revision and manifest digest
    /// and consumed once through the existing approval ledger, the durable
    /// execution claim, then serial execution. If any step fails, no unlink
    /// occurs and the run requires inspection — it is never replayed.
    pub fn confirm_purge_execution(
        &mut self,
        review_id: ReviewId,
        confirmer: impl FnOnce(&disk_steward_fixture_purge::PreparedPurgeReview) -> bool,
    ) -> DemoResult<disk_steward_fixture_purge::PurgeExecutionResult> {
        self.ensure_active()?;
        if self.uncertainty_across_stores()? {
            return Err(DemoError::NeedsInspection);
        }
        let prepared = self
            .purge
            .read_stored_review(&review_id)
            .map_err(map_purge)?;
        let now = self.checked_now()?;
        if now >= prepared.expires_at_ms {
            return Err(DemoError::ReviewExpired);
        }
        {
            let lifecycle = self.lifecycle.as_mut().ok_or(DemoError::NeedsInspection)?;
            self.purge
                .revalidate(lifecycle, &self.plan, &prepared)
                .map_err(map_purge)?;
        }
        if !confirmer(&prepared) {
            return Err(DemoError::ConfirmationCancelled);
        }
        let after = self.checked_now()?;
        if after >= prepared.expires_at_ms {
            return Err(DemoError::ReviewExpired);
        }
        {
            let lifecycle = self.lifecycle.as_mut().ok_or(DemoError::NeedsInspection)?;
            self.purge
                .revalidate(lifecycle, &self.plan, &prepared)
                .map_err(map_purge)?;
        }
        // Durable authority, in contract order: claim (execution row plus
        // pending child attempts) BEFORE any file mutation, then the core
        // approval consumed once through the existing ledger, then execution.
        let (execution_id, attempts) = self.purge.claim(&prepared, after).map_err(map_purge)?;
        let current_plan = self
            .plan
            .get_plan()
            .map_err(|_| DemoError::PlanUnavailable)?;
        // Pre-execution revalidation in its own scope so the borrow ends
        // before the clock read below.
        {
            let lifecycle = self.lifecycle.as_mut().ok_or(DemoError::NeedsInspection)?;
            self.purge
                .revalidate(lifecycle, &self.plan, &prepared)
                .map_err(map_purge)?;
        }
        // The core approval snapshot needs the CURRENT fixture identity
        // digests; take it fresh after the pre-execution revalidation.
        let session_snapshot = {
            let lifecycle = self.lifecycle.as_mut().ok_or(DemoError::NeedsInspection)?;
            lifecycle.snapshot().map_err(map_lifecycle)?
        };
        let snapshot = make_purge_core_snapshot(&prepared, &current_plan, &session_snapshot)?;
        let approval = Approval::issue(&snapshot, after as u64, prepared.expires_at_ms as u64)
            .ok_or(DemoError::InvalidReview)?;
        approval
            .validate(&snapshot, after as u64)
            .map_err(|_| DemoError::InvalidReview)?;
        let approval_id = approval.id();
        let approval_wire = wire(&approval_id)?;
        let execution_wire = wire(&execution_id)?;
        let core_manifest = core_manifest_digest_hex(&prepared)?;
        let ledger_result = self
            .ledger
            .insert_approval(&approval_wire, &core_manifest, prepared.expires_at_ms)
            .and_then(|()| {
                self.ledger
                    .consume_approval(&approval_wire, &core_manifest, after)
                    .map(|_| ())
            })
            .and_then(|()| {
                self.ledger
                    .record_intent_started(&execution_wire, &approval_wire, after)
                    .map(|_| ())
            });
        if ledger_result.is_err() {
            self.purge
                .mark_execution_uncertain(execution_id, &attempts, after)
                .map_err(map_purge)?;
            return Err(DemoError::LedgerUnavailable);
        }
        let execution_time = self.checked_now();
        let execution_check = match execution_time {
            Ok(time) => {
                let lifecycle = self.lifecycle.as_mut().ok_or(DemoError::NeedsInspection)?;
                self.purge
                    .revalidate(lifecycle, &self.plan, &prepared)
                    .map_err(map_purge)
                    .map(|()| time)
            }
            Err(error) => Err(error),
        };
        let execution_time = match execution_check {
            Ok(time) => time,
            Err(error) => {
                self.purge
                    .mark_execution_uncertain(execution_id, &attempts, after)
                    .map_err(map_purge)?;
                return Err(error);
            }
        };
        let _ = execution_time;
        let lifecycle = self.lifecycle.as_mut().ok_or(DemoError::NeedsInspection)?;
        let result = self
            .purge
            .execute_claimed_batch(lifecycle, execution_id, &prepared, attempts)
            .map_err(map_purge)?;
        Ok(result)
    }

    fn prepare(
        &mut self,
        expected: RevisionId,
        operation: &str,
        receipt: Option<ReceiptId>,
    ) -> DemoResult<PreparedDemoReview> {
        self.ensure_active()?;
        // Cross-store uncertainty consult: an open purge uncertainty blocks
        // new stage/restore work until inspection (contract fixture-purge-v1).
        if self.purge.has_uncertainty().map_err(map_purge)? {
            return Err(DemoError::NeedsInspection);
        }
        let session = self.ready_session()?;
        if self
            .last_execution()?
            .is_some_and(|execution| execution.state == "uncertain")
        {
            return Err(DemoError::NeedsInspection);
        }
        let plan = self
            .plan
            .get_plan()
            .map_err(|_| DemoError::PlanUnavailable)?;
        if plan.revision_id != expected {
            return Err(DemoError::StaleRevision);
        }
        let frozen = self.plan.freeze_review(expected).map_err(|e| match e {
            disk_steward_review_plan::StoreError::StaleRevision => DemoError::StaleRevision,
            disk_steward_review_plan::StoreError::NothingSelected => DemoError::NothingSelected,
            _ => DemoError::PlanUnavailable,
        })?;
        if frozen.kind != "non_executable_example" || frozen.execution_available {
            return Err(DemoError::InvalidReview);
        }
        let snapshot = self.snapshot()?;
        let selected: Vec<&FixtureSnapshot> = if operation == "stage" {
            snapshot
                .fixtures
                .iter()
                .filter(|f| {
                    plan.actions.iter().any(|a| {
                        a.id == f.action_id
                            && a.decision == disk_steward_review_plan::Decision::Include
                    })
                })
                .collect()
        } else {
            let r = receipt.ok_or(DemoError::InvalidReview)?;
            snapshot
                .fixtures
                .iter()
                .filter(|f| {
                    f.receipt_id == Some(r)
                        && plan.actions.iter().any(|a| {
                            a.id == f.action_id
                                && a.decision == disk_steward_review_plan::Decision::Include
                        })
                })
                .collect()
        };
        if selected.is_empty() {
            return Err(DemoError::NothingSelected);
        }
        for f in &selected {
            match (operation, &f.status) {
                ("stage", FixtureStatus::Available | FixtureStatus::Restored)
                    if f.receipt_id.is_none() || f.status == FixtureStatus::Restored => {}
                ("restore", FixtureStatus::Staged) if f.receipt_id.is_some() => {}
                _ => return Err(DemoError::FixtureUnavailable),
            }
        }
        let items: Vec<PreparedItem> = selected
            .iter()
            .map(|f| {
                let a = plan
                    .actions
                    .iter()
                    .find(|a| a.id == f.action_id)
                    .ok_or(DemoError::InvalidReview)?;
                Ok(PreparedItem {
                    action_id: f.action_id,
                    label: a.label.clone(),
                    bytes: f.bytes,
                })
            })
            .collect::<DemoResult<_>>()?;
        let selected_bytes = items.iter().try_fold(0_u64, |n, x| {
            n.checked_add(x.bytes).ok_or(DemoError::InvalidReview)
        })?;
        let now = self.checked_now()?;
        let expires = now
            .checked_add(REVIEW_TTL_MS)
            .ok_or(DemoError::ClockReversed)?;
        let review_id = ReviewId::new();
        let frozen_hash = frozen.manifest_sha256.clone();
        let manifest = FileManifest {
            contract: "disk-steward-disposable-file-review-v1".into(),
            manifest_version: MANIFEST_VERSION,
            adapter_version: ADAPTER_VERSION.into(),
            session_id: session,
            scope_id: ScopeId::new(),
            plan_id: plan.plan_id,
            revision_id: expected,
            prepared_review_id: review_id,
            created_at_ms: now,
            expires_at_ms: expires,
            frozen_review_id: frozen.review_id,
            frozen_review_sha256: frozen_hash,
            operation: operation.into(),
            receipt_id: receipt,
            fixtures: snapshot.fixtures,
            selected: items.iter().map(|i| i.action_id).collect(),
        };
        let digest = hash_json(&manifest)?;
        let prepared = PreparedDemoReview {
            schema_version: 1,
            kind: "disposable_file_review".into(),
            review_id,
            plan_id: plan.plan_id,
            revision_id: expected,
            session_id: session,
            operation: operation.into(),
            items,
            selected_bytes,
            manifest_sha256: digest.clone(),
            expires_at_ms: expires,
        };
        let json = bounded_json(&manifest)?;
        self.control.execute("INSERT INTO prepared_reviews(review_id,manifest_json,manifest_sha256,expires_at_ms,claimed) VALUES(?1,?2,?3,?4,0)", params![wire(&review_id)?, json, digest, expires]).map_err(map_sql)?;
        Ok(prepared)
    }

    fn read_review(&self, id: ReviewId) -> DemoResult<(PreparedDemoReview, FileManifest, String)> {
        // Prepared DTO is deterministically reconstructed from the immutable manifest and stored row.
        let row: Option<(String,String,i64,i64)> = self.control.query_row("SELECT manifest_json,manifest_sha256,expires_at_ms,claimed FROM prepared_reviews WHERE review_id=?1", [wire(&id)?], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(map_sql)?;
        let (json, digest, expires, claimed) = row.ok_or(DemoError::InvalidReview)?;
        if claimed != 0 {
            return Err(DemoError::ReviewReused);
        }
        if json.len() > MAX_JSON {
            return Err(DemoError::CorruptData);
        }
        let manifest: FileManifest =
            serde_json::from_str(&json).map_err(|_| DemoError::CorruptData)?;
        if hash_json(&manifest)? != digest
            || manifest.prepared_review_id != id
            || manifest.expires_at_ms != expires
            || manifest.created_at_ms < 0
            || manifest.expires_at_ms.checked_sub(manifest.created_at_ms) != Some(REVIEW_TTL_MS)
        {
            return Err(DemoError::CorruptData);
        }
        let plan = self
            .plan
            .get_plan()
            .map_err(|_| DemoError::PlanUnavailable)?;
        let items = manifest
            .selected
            .iter()
            .map(|action| {
                let f = manifest
                    .fixtures
                    .iter()
                    .find(|f| f.action_id == *action)
                    .ok_or(DemoError::CorruptData)?;
                let a = plan
                    .actions
                    .iter()
                    .find(|a| a.id == *action)
                    .ok_or(DemoError::CorruptData)?;
                Ok(PreparedItem {
                    action_id: *action,
                    label: a.label.clone(),
                    bytes: f.bytes,
                })
            })
            .collect::<DemoResult<Vec<_>>>()?;
        let selected_bytes = items.iter().try_fold(0_u64, |n, x| {
            n.checked_add(x.bytes).ok_or(DemoError::CorruptData)
        })?;
        let prepared = PreparedDemoReview {
            schema_version: 1,
            kind: "disposable_file_review".into(),
            review_id: id,
            plan_id: manifest.plan_id,
            revision_id: manifest.revision_id,
            session_id: manifest.session_id,
            operation: manifest.operation.clone(),
            items,
            selected_bytes,
            manifest_sha256: digest.clone(),
            expires_at_ms: expires,
        };
        Ok((prepared, manifest, digest))
    }

    fn revalidate(
        &mut self,
        prepared: &PreparedDemoReview,
        manifest: &FileManifest,
        digest: &str,
        now: i64,
    ) -> DemoResult<()> {
        if prepared.kind != "disposable_file_review"
            || prepared.schema_version != 1
            || prepared.manifest_sha256 != digest
            || hash_json(manifest)? != digest
            || manifest.prepared_review_id != prepared.review_id
            || manifest.expires_at_ms != prepared.expires_at_ms
            || manifest.expires_at_ms.checked_sub(manifest.created_at_ms) != Some(REVIEW_TTL_MS)
            || manifest.created_at_ms > now
        {
            return Err(DemoError::InvalidReview);
        }
        let plan = self
            .plan
            .get_plan()
            .map_err(|_| DemoError::PlanUnavailable)?;
        if plan.revision_id != prepared.revision_id || plan.plan_id != prepared.plan_id {
            return Err(DemoError::StaleRevision);
        }
        let frozen = self
            .plan
            .freeze_review(plan.revision_id)
            .map_err(|_| DemoError::PlanUnavailable)?;
        if frozen.kind != "non_executable_example"
            || frozen.execution_available
            || frozen.review_id != manifest.frozen_review_id
            || frozen.manifest_sha256 != manifest.frozen_review_sha256
        {
            return Err(DemoError::StaleRevision);
        }
        validate_file_manifest(manifest, prepared, &plan, &frozen)?;
        if self.ready_session()? != manifest.session_id
            || manifest.operation != prepared.operation
            || prepared.expires_at_ms <= now
        {
            return Err(DemoError::InvalidReview);
        }
        let current = self.snapshot()?;
        if current.fixtures != manifest.fixtures {
            return Err(DemoError::FixtureChanged);
        }
        let selected: Vec<ActionId> = prepared.items.iter().map(|i| i.action_id).collect();
        if selected != manifest.selected {
            return Err(DemoError::InvalidReview);
        }
        Ok(())
    }

    fn execute_batch(
        &mut self,
        prepared: &PreparedDemoReview,
        execution: ExecutionId,
        attempts: &[AttemptId],
        _now: i64,
    ) -> DemoResult<DemoExecutionResult> {
        let mut results = Vec::new();
        let mut all_ok = true;
        let mut uncertain = false;
        let mut staged_bytes = 0_u64;
        let mut restored_bytes = 0_u64;
        for (index, (item, attempt)) in prepared.items.iter().zip(attempts).enumerate() {
            let prior = self.snapshot_for_item(item.action_id)?;
            let lifecycle = self.lifecycle.as_mut().ok_or(DemoError::NeedsInspection)?;
            let result = match prepared.operation.as_str() {
                "stage" => lifecycle.stage(item.action_id, *attempt).map(Some),
                "restore" => prior
                    .receipt_id
                    .ok_or(LifecycleError::UnknownReceipt)
                    .and_then(|receipt| lifecycle.restore(receipt, *attempt).map(|_| None)),
                _ => return Err(DemoError::InvalidReview),
            };
            let (state, receipt) = match result {
                Ok(Some(r)) => {
                    staged_bytes = staged_bytes.saturating_add(item.bytes);
                    ("staged", Some(r))
                }
                Ok(None) => {
                    restored_bytes = restored_bytes.saturating_add(item.bytes);
                    ("restored", prior.receipt_id)
                }
                Err(LifecycleError::Collision) => {
                    all_ok = false;
                    ("collision", prior.receipt_id)
                }
                Err(LifecycleError::Uncertain | LifecycleError::Changed) => {
                    all_ok = false;
                    uncertain = true;
                    ("uncertain", prior.receipt_id)
                }
                Err(_) => {
                    all_ok = false;
                    uncertain = true;
                    ("uncertain", prior.receipt_id)
                }
            };
            let rw = receipt.as_ref().map(wire).transpose()?;
            self.control.execute("UPDATE child_attempts SET state=?1,receipt_id=?2 WHERE attempt_id=?3 AND state='pending'", params![state, rw, wire(attempt)?]).map_err(map_sql)?;
            results.push(ExecutionItem {
                action_id: item.action_id,
                receipt_id: receipt,
                status: state.into(),
            });
            if uncertain {
                for (remaining, remaining_attempt) in
                    prepared.items.iter().zip(attempts).skip(index + 1)
                {
                    // Keep the durable child state conservatively blocked. The
                    // batch summary distinguishes files known not to be tried.
                    let changed = self.control.execute(
                        "UPDATE child_attempts SET state='uncertain' WHERE attempt_id=?1 AND state='pending'",
                        [wire(remaining_attempt)?],
                    ).map_err(map_sql)?;
                    if changed != 1 {
                        return Err(DemoError::ExecutionUncertain);
                    }
                    results.push(ExecutionItem {
                        action_id: remaining.action_id,
                        receipt_id: None,
                        status: "not_attempted".into(),
                    });
                }
                break;
            }
        }
        let state = if uncertain {
            "uncertain"
        } else if all_ok {
            "finished"
        } else {
            "partial"
        };
        let finished = (self.now)();
        let summary = ExecutionSummary {
            execution_id: execution,
            operation: prepared.operation.clone(),
            state: state.into(),
            items: results,
            staged_bytes,
            restored_bytes,
            recovered_bytes: 0,
        };
        let json = bounded_json(&summary)?;
        let tx = self
            .control
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        tx.execute("UPDATE executions SET state=?1,batch_json=?2,finished_at_ms=?3 WHERE execution_id=?4 AND state='started'", params![state,json,finished,wire(&execution)?]).map_err(map_sql)?;
        tx.execute("UPDATE control SET active_execution_id=NULL WHERE singleton=1 AND active_execution_id=?1", [wire(&execution)?]).map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        let ledger_result = match state {
            "finished" => self
                .ledger
                .record_intent_finished(&wire(&execution)?, finished),
            _ => self
                .ledger
                .record_intent_uncertain(&wire(&execution)?, finished),
        };
        if ledger_result.is_err() {
            let mut uncertain = summary.clone();
            uncertain.state = "uncertain".into();
            let stored = bounded_json(&uncertain)?;
            if self.control.execute("UPDATE executions SET state='uncertain',batch_json=?1 WHERE execution_id=?2 AND state IN ('finished','partial','uncertain')", params![stored,wire(&execution)?]).is_err() {
                self.poisoned = true;
                self.lease.take();
                return Err(DemoError::Storage);
            }
            let _ = self
                .ledger
                .record_intent_uncertain(&wire(&execution)?, finished);
            return Err(DemoError::LedgerUnavailable);
        }
        Ok(summary)
    }

    fn mark_claim_uncertain(
        &mut self,
        prepared: &PreparedDemoReview,
        execution: ExecutionId,
        attempts: &[AttemptId],
        at_ms: i64,
    ) -> DemoResult<()> {
        let mut items = Vec::new();
        let mut staged_bytes = 0_u64;
        let mut restored_bytes = 0_u64;
        for (item, attempt) in prepared.items.iter().zip(attempts) {
            let row: (String, Option<String>, String) = match self.control.query_row(
                "SELECT action_id,receipt_id,state FROM child_attempts WHERE attempt_id=?1",
                [wire(attempt)?],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ) {
                Ok(row) => row,
                Err(_) => return Err(self.poison_and_release()),
            };
            let (action_wire, receipt_wire, stored_state) = row;
            let action_id: ActionId = match serde_json::from_str(&action_wire) {
                Ok(id) => id,
                Err(_) => return Err(self.poison_and_release()),
            };
            if action_id != item.action_id {
                return Err(self.poison_and_release());
            }
            let receipt_id = match receipt_wire
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
            {
                Ok(id) => id,
                Err(_) => return Err(self.poison_and_release()),
            };
            let status = if stored_state == "pending" {
                "uncertain"
            } else {
                stored_state.as_str()
            };
            if status == "staged" {
                staged_bytes = staged_bytes.saturating_add(item.bytes);
            }
            if status == "restored" {
                restored_bytes = restored_bytes.saturating_add(item.bytes);
            }
            items.push(ExecutionItem {
                action_id,
                receipt_id,
                status: status.into(),
            });
        }
        let summary = ExecutionSummary {
            execution_id: execution,
            operation: prepared.operation.clone(),
            state: "uncertain".into(),
            items,
            staged_bytes,
            restored_bytes,
            recovered_bytes: 0,
        };
        let result = (|| -> rusqlite::Result<()> {
            let tx = self
                .control
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for attempt in attempts {
                tx.execute("UPDATE child_attempts SET state='uncertain' WHERE attempt_id=?1 AND state='pending'", [wire(attempt).map_err(|_| rusqlite::Error::InvalidQuery)?])?;
            }
            tx.execute("UPDATE executions SET state='uncertain',batch_json=?1,finished_at_ms=COALESCE(finished_at_ms,?2) WHERE execution_id=?3 AND state IN ('started','finished','partial','uncertain')", params![serde_json::to_string(&summary).map_err(|_| rusqlite::Error::InvalidQuery)?,at_ms,wire(&execution).map_err(|_| rusqlite::Error::InvalidQuery)?])?;
            tx.execute("UPDATE control SET active_execution_id=NULL WHERE singleton=1 AND active_execution_id=?1", [wire(&execution).map_err(|_| rusqlite::Error::InvalidQuery)?])?;
            tx.commit()
        })();
        if result.is_err() {
            self.poisoned = true;
            self.lease.take();
            return Err(DemoError::Storage);
        }
        let _ = self
            .ledger
            .record_intent_uncertain(&wire(&execution)?, at_ms);
        Ok(())
    }

    fn poison_and_release(&mut self) -> DemoError {
        self.poisoned = true;
        self.lease.take();
        DemoError::Storage
    }

    fn snapshot_for_item(&mut self, action: ActionId) -> DemoResult<FixtureSnapshot> {
        self.snapshot()?
            .fixtures
            .into_iter()
            .find(|f| f.action_id == action)
            .ok_or(DemoError::FixtureUnavailable)
    }
    fn ensure_active(&self) -> DemoResult<()> {
        if self.poisoned || self.lease.is_none() {
            Err(DemoError::NeedsInspection)
        } else {
            Ok(())
        }
    }
    fn snapshot(&mut self) -> DemoResult<SessionSnapshot> {
        self.lifecycle
            .as_ref()
            .ok_or(DemoError::NeedsInspection)?
            .snapshot()
            .map_err(map_lifecycle)
    }
    fn ready_session(&mut self) -> DemoResult<SessionId> {
        let (state, sid): (String, Option<String>) = self
            .control
            .query_row(
                "SELECT creation_state,session_id FROM control WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(map_sql)?;
        if state != "ready" {
            return Err(if state == "none" {
                DemoError::SessionUnavailable
            } else {
                DemoError::NeedsInspection
            });
        }
        if self.lifecycle.is_none() {
            // Contract fixture-runtime-schema-v2: the owner lease must be held
            // for the complete reopen, since reopen may perform the v1->v2
            // write migration. A poisoned service has released the lease, so
            // refuse instead of migrating leaseless. (With the lifecycle
            // handle still held, status() stays usable after a poison.)
            self.ensure_active()?;
            let id: SessionId = serde_json::from_str(&sid.ok_or(DemoError::CorruptData)?)
                .map_err(|_| DemoError::CorruptData)?;
            let (lifecycle, _) = FixtureLifecycle::reopen(&self.root, id).map_err(map_lifecycle)?;
            self.lifecycle = Some(lifecycle);
        }
        self.lifecycle
            .as_ref()
            .map(FixtureLifecycle::session_id)
            .ok_or(DemoError::NeedsInspection)
    }
    fn checked_now(&mut self) -> DemoResult<i64> {
        let now = (self.now)();
        let tx = self
            .control
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        let last: i64 = tx
            .query_row(
                "SELECT last_now_ms FROM clock_guard WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        if now < last {
            return Err(DemoError::ClockReversed);
        }
        tx.execute(
            "UPDATE clock_guard SET last_now_ms=?1 WHERE singleton=1",
            [now],
        )
        .map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        Ok(now)
    }
    fn last_execution(&self) -> DemoResult<Option<ExecutionSummary>> {
        let json: Option<String> = self
            .control
            .query_row(
                "SELECT batch_json FROM executions ORDER BY started_at_ms DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_sql)?;
        json.map(|s| {
            if s.len() > MAX_JSON {
                return Err(DemoError::CorruptData);
            }
            serde_json::from_str(&s).map_err(|_| DemoError::CorruptData)
        })
        .transpose()
    }
    fn recover_started_work(&mut self) -> DemoResult<()> {
        let started: Vec<(String, String, String, String, String)> = {
            let mut query = self.control.prepare(
                "SELECT execution_id,review_id,approval_id,operation,batch_json FROM executions WHERE state='started' LIMIT 2",
            ).map_err(map_sql)?;
            let rows = query
                .query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })
                .map_err(map_sql)?;
            rows.collect::<Result<_, _>>().map_err(map_sql)?
        };
        if started.len() > 1 {
            return Err(DemoError::CorruptData);
        }
        let active: Option<String> = self
            .control
            .query_row(
                "SELECT active_execution_id FROM control WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(map_sql)?;
        if started.is_empty() {
            if active.is_some() {
                return Err(DemoError::CorruptData);
            }
            return Ok(());
        }
        let (execution, review, approval, operation, batch_json) =
            started.into_iter().next().ok_or(DemoError::CorruptData)?;
        if batch_json.len() > MAX_JSON {
            return Err(DemoError::CorruptData);
        }
        if active.as_deref() != Some(execution.as_str()) {
            return Err(DemoError::CorruptData);
        }
        let id: ExecutionId =
            serde_json::from_str(&execution).map_err(|_| DemoError::CorruptData)?;
        let review_id: ReviewId =
            serde_json::from_str(&review).map_err(|_| DemoError::CorruptData)?;
        let approval_id: ApprovalId =
            serde_json::from_str(&approval).map_err(|_| DemoError::CorruptData)?;
        if operation != "stage" && operation != "restore" {
            return Err(DemoError::CorruptData);
        }
        let binding: BatchBinding =
            serde_json::from_str(&batch_json).map_err(|_| DemoError::CorruptData)?;
        if binding.execution_id != id
            || binding.review_id != review_id
            || binding.approval_id != approval_id
            || binding.attempts.is_empty()
            || binding.attempts.len() > 5
            || !is_lower_sha256(&binding.digest)
        {
            return Err(DemoError::CorruptData);
        }
        let unique_attempts: std::collections::HashSet<AttemptId> =
            binding.attempts.iter().copied().collect();
        if unique_attempts.len() != binding.attempts.len() {
            return Err(DemoError::CorruptData);
        }
        let child_attempt_ids: Vec<String> = {
            let mut query = self
                .control
                .prepare("SELECT attempt_id FROM child_attempts WHERE execution_id=?1 LIMIT 6")
                .map_err(map_sql)?;
            let rows = query
                .query_map([&execution], |r| r.get(0))
                .map_err(map_sql)?;
            rows.collect::<Result<_, _>>().map_err(map_sql)?
        };
        if child_attempt_ids.len() != binding.attempts.len() {
            return Err(DemoError::CorruptData);
        }
        {
            let mut actions = std::collections::HashSet::new();
            for attempt in &binding.attempts {
                let action_wire: String = self.control.query_row("SELECT action_id FROM child_attempts WHERE attempt_id=?1 AND execution_id=?2", params![wire(attempt)?,execution], |r| r.get(0)).map_err(map_sql)?;
                let action: ActionId =
                    serde_json::from_str(&action_wire).map_err(|_| DemoError::CorruptData)?;
                if !actions.insert(action) {
                    return Err(DemoError::CorruptData);
                }
            }
        }
        let mut items = Vec::new();
        let mut attempts_to_update = Vec::new();
        let plan = self
            .plan
            .get_plan()
            .map_err(|_| DemoError::PlanUnavailable)?;
        let mut staged_bytes = 0_u64;
        let mut restored_bytes = 0_u64;
        for attempt in &binding.attempts {
            let attempt_id = wire(attempt)?;
            let row: Option<(String, Option<String>, String)> = self
                .control
                .query_row(
                    "SELECT action_id,receipt_id,state FROM child_attempts WHERE attempt_id=?1 AND execution_id=?2",
                    params![attempt_id,execution],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(map_sql)?;
            let (action_wire, receipt_wire, prior_state) = row.ok_or(DemoError::CorruptData)?;
            let action_id: ActionId =
                serde_json::from_str(&action_wire).map_err(|_| DemoError::CorruptData)?;
            let action = plan
                .actions
                .iter()
                .find(|action| action.id == action_id)
                .ok_or(DemoError::CorruptData)?;
            let receipt_id = receipt_wire
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| DemoError::CorruptData)?;
            if prior_state == "pending" {
                attempts_to_update.push(attempt_id);
            }
            let state = if prior_state == "pending" {
                "uncertain"
            } else {
                prior_state.as_str()
            };
            if prior_state == "staged" && operation == "stage" {
                staged_bytes = staged_bytes
                    .checked_add(action.bytes)
                    .ok_or(DemoError::CorruptData)?;
            }
            if prior_state == "restored" && operation == "restore" {
                restored_bytes = restored_bytes
                    .checked_add(action.bytes)
                    .ok_or(DemoError::CorruptData)?;
            }
            items.push(ExecutionItem {
                action_id,
                receipt_id,
                status: state.into(),
            });
        }
        let summary = ExecutionSummary {
            execution_id: id,
            operation,
            state: "uncertain".into(),
            items,
            staged_bytes,
            restored_bytes,
            recovered_bytes: 0,
        };
        let now = (self.now)();
        let tx = self
            .control
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sql)?;
        for attempt_id in attempts_to_update {
            tx.execute("UPDATE child_attempts SET state='uncertain' WHERE attempt_id=?1 AND execution_id=?2 AND state='pending'", params![attempt_id,execution]).map_err(map_sql)?;
        }
        tx.execute("UPDATE executions SET state='uncertain',batch_json=?1,finished_at_ms=?2 WHERE execution_id=?3 AND state='started'", params![bounded_json(&summary)?, now, execution]).map_err(map_sql)?;
        tx.execute("UPDATE control SET active_execution_id=NULL WHERE singleton=1 AND active_execution_id=?1", [&execution]).map_err(map_sql)?;
        tx.commit().map_err(map_sql)?;
        let _ = self.ledger.record_intent_uncertain(&wire(&id)?, now);
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchBinding {
    execution_id: ExecutionId,
    approval_id: ApprovalId,
    attempts: Vec<AttemptId>,
    review_id: ReviewId,
    digest: String,
}

/// The core approval digest for a purge review: the same construction as the
/// demo review's — the manifest digest bytes hashed, hex-encoded — binding
/// the consumed approval to the exact frozen purge manifest.
fn core_manifest_digest_hex(
    prepared: &disk_steward_fixture_purge::PreparedPurgeReview,
) -> DemoResult<String> {
    Ok(hex(&sha_bytes(prepared.manifest_sha256.as_bytes())))
}

/// The core plan snapshot for a purge confirmation: one ActionBinding per
/// frozen target (permanently-remove operation, target digest over
/// session/action/identity, preconditions over content/bytes/status/receipt),
/// bound to the frozen plan/revision and the purge manifest digest. The
/// identity digests transitively bind the registered device/inode and
/// directory bindings; no path or raw inode enters the approval.
fn make_purge_core_snapshot(
    prepared: &disk_steward_fixture_purge::PreparedPurgeReview,
    plan: &PlanView,
    snapshot: &SessionSnapshot,
) -> DemoResult<PlanSnapshot> {
    let scope = ScopeId::new();
    let mut selected = Vec::new();
    for target in &prepared.targets {
        let fixture = snapshot
            .fixtures
            .iter()
            .find(|f| f.action_id == target.action_id)
            .ok_or(DemoError::InvalidReview)?;
        let target_digest = sha_bytes(
            format!(
                "{}:{}:{}",
                wire(&prepared.session_id)?,
                wire(&target.action_id)?,
                fixture.identity_sha256
            )
            .as_bytes(),
        );
        let pre = sha_bytes(
            format!(
                "{}:{}:{}:{}",
                fixture.sha256,
                fixture.bytes,
                fixture.status_as_text(),
                fixture
                    .receipt_id
                    .as_ref()
                    .map(wire)
                    .transpose()?
                    .unwrap_or_default()
            )
            .as_bytes(),
        );
        selected.push(ActionBinding::new(
            target.action_id,
            scope,
            SupportedOperation::PermanentlyRemoveStaged,
            Digest::from_bytes(target_digest),
            Digest::from_bytes(pre),
        ));
    }
    let manifest_digest = sha_bytes(prepared.manifest_sha256.as_bytes());
    let preconditions =
        sha_bytes(format!("{}:{}", wire(&prepared.revision_id)?, plan.revision_number).as_bytes());
    Ok(PlanSnapshot::new(PlanSnapshotParts {
        plan: prepared.plan_id,
        revision: prepared.revision_id,
        revision_number: plan.revision_number,
        manifest: Digest::from_bytes(manifest_digest),
        preconditions: Digest::from_bytes(preconditions),
        scopes: vec![ScopeSnapshot::new(scope, 1, ScopeAccess::Granted)],
        adapter_versions: vec![
            ("fixture-lifecycle".into(), "2".into()),
            ("fixture-purge".into(), "1".into()),
        ],
        actions: selected,
    }))
}

fn make_core_snapshot(
    manifest: &FileManifest,
    prepared: &PreparedDemoReview,
    plan: &PlanView,
) -> DemoResult<PlanSnapshot> {
    let mut selected = Vec::new();
    for item in &prepared.items {
        let fixture = manifest
            .fixtures
            .iter()
            .find(|f| f.action_id == item.action_id)
            .ok_or(DemoError::InvalidReview)?;
        let target = sha_bytes(
            format!(
                "{}:{}:{}",
                wire(&manifest.session_id)?,
                wire(&item.action_id)?,
                fixture.identity_sha256
            )
            .as_bytes(),
        );
        let pre = sha_bytes(
            format!(
                "{}:{}:{}:{}",
                fixture.sha256,
                fixture.bytes,
                fixture.status_as_text(),
                fixture
                    .receipt_id
                    .map(|r| wire(&r))
                    .transpose()?
                    .unwrap_or_default()
            )
            .as_bytes(),
        );
        selected.push(ActionBinding::new(
            item.action_id,
            manifest.scope_id,
            if prepared.operation == "stage" {
                SupportedOperation::StageToRecovery
            } else {
                SupportedOperation::RestoreFromRecovery
            },
            Digest::from_bytes(target),
            Digest::from_bytes(pre),
        ));
    }
    let man = sha_bytes(prepared.manifest_sha256.as_bytes());
    let pre = sha_bytes(manifest.frozen_review_sha256.as_bytes());
    Ok(PlanSnapshot::new(PlanSnapshotParts {
        plan: prepared.plan_id,
        revision: prepared.revision_id,
        revision_number: plan.revision_number,
        manifest: Digest::from_bytes(man),
        preconditions: Digest::from_bytes(pre),
        scopes: vec![ScopeSnapshot::new(
            manifest.scope_id,
            1,
            ScopeAccess::Granted,
        )],
        adapter_versions: vec![("fixture-lifecycle".into(), ADAPTER_VERSION.into())],
        actions: selected,
    }))
}

fn validate_file_manifest(
    manifest: &FileManifest,
    prepared: &PreparedDemoReview,
    plan: &PlanView,
    frozen: &disk_steward_review_plan::FrozenReview,
) -> DemoResult<()> {
    if manifest.contract != "disk-steward-disposable-file-review-v1"
        || manifest.manifest_version != MANIFEST_VERSION
        || manifest.adapter_version != ADAPTER_VERSION
        || manifest.session_id != prepared.session_id
        || manifest.plan_id != prepared.plan_id
        || manifest.revision_id != prepared.revision_id
        || manifest.prepared_review_id != prepared.review_id
        || manifest.frozen_review_id != frozen.review_id
        || manifest.frozen_review_sha256 != frozen.manifest_sha256
        || manifest.operation != prepared.operation
        || plan.actions.len() != 5
        || manifest.fixtures.len() != 5
    {
        return Err(DemoError::InvalidReview);
    }
    for (fixture, action) in manifest.fixtures.iter().zip(&plan.actions) {
        if fixture.action_id != action.id
            || fixture.bytes != action.bytes
            || !is_lower_sha256(&fixture.sha256)
            || !is_lower_sha256(&fixture.identity_sha256)
        {
            return Err(DemoError::InvalidReview);
        }
    }
    let expected: Vec<ActionId> = match manifest.operation.as_str() {
        "stage" => {
            if manifest.receipt_id.is_some() {
                return Err(DemoError::InvalidReview);
            }
            plan.actions
                .iter()
                .filter(|action| action.decision == disk_steward_review_plan::Decision::Include)
                .map(|action| action.id)
                .collect()
        }
        "restore" => {
            let receipt = manifest.receipt_id.ok_or(DemoError::InvalidReview)?;
            let selected: Vec<ActionId> = manifest
                .fixtures
                .iter()
                .filter(|fixture| {
                    fixture.receipt_id == Some(receipt) && fixture.status == FixtureStatus::Staged
                })
                .filter_map(|fixture| {
                    plan.actions
                        .iter()
                        .find(|action| {
                            action.id == fixture.action_id
                                && action.decision == disk_steward_review_plan::Decision::Include
                        })
                        .map(|action| action.id)
                })
                .collect();
            if selected.len() != 1 {
                return Err(DemoError::InvalidReview);
            }
            selected
        }
        _ => return Err(DemoError::InvalidReview),
    };
    if expected.is_empty() || manifest.selected != expected {
        return Err(DemoError::InvalidReview);
    }
    for action_id in &expected {
        let fixture = manifest
            .fixtures
            .iter()
            .find(|fixture| fixture.action_id == *action_id)
            .ok_or(DemoError::InvalidReview)?;
        match manifest.operation.as_str() {
            "stage"
                if !matches!(
                    fixture.status,
                    FixtureStatus::Available | FixtureStatus::Restored
                ) =>
            {
                return Err(DemoError::InvalidReview)
            }
            "restore"
                if fixture.status != FixtureStatus::Staged
                    || fixture.receipt_id != manifest.receipt_id =>
            {
                return Err(DemoError::InvalidReview)
            }
            _ => {}
        }
    }
    if prepared.items.len() != expected.len()
        || prepared
            .items
            .iter()
            .map(|item| item.action_id)
            .collect::<Vec<_>>()
            != expected
    {
        return Err(DemoError::InvalidReview);
    }
    let mut selected_bytes = 0_u64;
    for item in &prepared.items {
        let action = plan
            .actions
            .iter()
            .find(|action| action.id == item.action_id)
            .ok_or(DemoError::InvalidReview)?;
        if item.label != action.label || item.bytes != action.bytes {
            return Err(DemoError::InvalidReview);
        }
        selected_bytes = selected_bytes
            .checked_add(item.bytes)
            .ok_or(DemoError::InvalidReview)?;
    }
    if selected_bytes != prepared.selected_bytes {
        return Err(DemoError::InvalidReview);
    }
    Ok(())
}

trait FixtureStatusText {
    fn status_as_text(&self) -> &'static str;
}
impl FixtureStatusText for FixtureSnapshot {
    fn status_as_text(&self) -> &'static str {
        match self.status {
            FixtureStatus::Available => "available",
            FixtureStatus::Staged => "staged",
            FixtureStatus::Restored => "restored",
            FixtureStatus::PermanentlyRemoved => "permanently_removed",
            FixtureStatus::Uncertain => "uncertain",
            FixtureStatus::Changed => "changed",
        }
    }
}
fn manifest_digest_for_core(
    _manifest: &FileManifest,
    prepared: &PreparedDemoReview,
) -> DemoResult<String> {
    Ok(hex(&sha_bytes(prepared.manifest_sha256.as_bytes())))
}
fn initialize_control(c: &Connection) -> rusqlite::Result<()> {
    let app: i64 = c.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let ver: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let count: i64 = c.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT GLOB 'sqlite_*'",
        [],
        |r| r.get(0),
    )?;
    if app == 0 && ver == 0 && count == 0 {
        c.execute_batch(&format!(
            "{CONTROL_SCHEMA};{CLOCK_SCHEMA};{REVIEWS_SCHEMA};{EXEC_SCHEMA};{ATTEMPTS_SCHEMA};"
        ))?;
        c.execute("INSERT INTO control VALUES(1,NULL,'none',NULL)", [])?;
        c.execute("INSERT INTO clock_guard VALUES(1,0)", [])?;
        c.execute_batch(&format!(
            "PRAGMA application_id={APP_ID};PRAGMA user_version={SCHEMA_VERSION};"
        ))?;
        return Ok(());
    }
    if app != APP_ID || ver != SCHEMA_VERSION || count != 5 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    for (name, sql) in [
        ("control", CONTROL_SCHEMA),
        ("clock_guard", CLOCK_SCHEMA),
        ("prepared_reviews", REVIEWS_SCHEMA),
        ("executions", EXEC_SCHEMA),
        ("child_attempts", ATTEMPTS_SCHEMA),
    ] {
        let actual: Option<String> = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        if actual.as_deref().map(norm_sql) != Some(norm_sql(sql)) {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    Ok(())
}
fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let name = path
        .file_name()
        .ok_or_else(|| rusqlite::Error::InvalidPath(path.to_owned()))?;
    let parent = fs::canonicalize(path.parent().unwrap_or(Path::new(".")))
        .map_err(|_| rusqlite::Error::InvalidPath(path.to_owned()))?;
    let db = Connection::open_with_flags(
        parent.join(name),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(db)
}
#[cfg(unix)]
fn acquire_lease(root: &Path) -> DemoResult<std::fs::File> {
    use std::os::fd::OwnedFd;
    let path = root.join(".demo-execution-v1.lock");
    let descriptor: OwnedFd = rustix::fs::open(
        &path,
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .map_err(|_| DemoError::Storage)?;
    let file = std::fs::File::from(descriptor);
    if !file.metadata().map_err(|_| DemoError::Storage)?.is_file() {
        return Err(DemoError::Storage);
    }
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|_| DemoError::Storage)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(DemoError::Busy),
        Err(std::fs::TryLockError::Error(_)) => Err(DemoError::Storage),
    }
}
#[cfg(not(unix))]
fn acquire_lease(_root: &Path) -> DemoResult<std::fs::File> {
    // The disposable-file adapter is currently supported only on Unix hosts.
    Err(DemoError::SessionUnavailable)
}
fn norm_sql(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != ';')
        .flat_map(char::to_lowercase)
        .collect()
}
fn hash_json<T: Serialize>(v: &T) -> DemoResult<String> {
    Ok(format!("{:x}", Sha256::digest(bounded_json(v)?.as_bytes())))
}
fn bounded_json<T: Serialize>(v: &T) -> DemoResult<String> {
    let s = serde_json::to_string(v).map_err(|_| DemoError::CorruptData)?;
    if s.len() > MAX_JSON {
        Err(DemoError::CorruptData)
    } else {
        Ok(s)
    }
}
fn wire<T: Serialize>(v: &T) -> DemoResult<String> {
    serde_json::to_string(v).map_err(|_| DemoError::CorruptData)
}
fn decode_sha256(s: &str) -> DemoResult<[u8; 32]> {
    if s.len() != 64 {
        return Err(DemoError::CorruptData);
    }
    let mut out = [0; 32];
    for (i, p) in s.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        out[i] = u8::from_str_radix(
            std::str::from_utf8(p).map_err(|_| DemoError::CorruptData)?,
            16,
        )
        .map_err(|_| DemoError::CorruptData)?;
    }
    Ok(out)
}
fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn sha_bytes(v: &[u8]) -> [u8; 32] {
    Sha256::digest(v).into()
}
fn hex(v: &[u8]) -> String {
    v.iter().map(|b| format!("{b:02x}")).collect()
}
fn wall_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
fn map_sql(_: rusqlite::Error) -> DemoError {
    DemoError::Storage
}
fn map_lifecycle(e: LifecycleError) -> DemoError {
    match e {
        LifecycleError::Changed => DemoError::FixtureChanged,
        LifecycleError::Uncertain => DemoError::NeedsInspection,
        _ => DemoError::FixtureUnavailable,
    }
}
fn map_purge(e: disk_steward_fixture_purge::PurgeError) -> DemoError {
    use disk_steward_fixture_purge::PurgeError;
    match e {
        PurgeError::StaleRevision => DemoError::StaleRevision,
        PurgeError::ReviewExpired => DemoError::ReviewExpired,
        PurgeError::ReviewReplayed => DemoError::ReviewReused,
        PurgeError::ConfirmationDeclined => DemoError::ConfirmationCancelled,
        PurgeError::ChangedTarget => DemoError::FixtureChanged,
        PurgeError::UnsupportedSchema => DemoError::CorruptData,
        PurgeError::BusyOwner => DemoError::Busy,
        PurgeError::LedgerUnavailable => DemoError::LedgerUnavailable,
        PurgeError::StorageFailure => DemoError::Storage,
        PurgeError::UncertainOutcome => DemoError::ExecutionUncertain,
        PurgeError::InvalidInput => DemoError::InvalidReview,
        PurgeError::UntrustedDirectory => DemoError::Storage,
    }
}
fn map_fixture_status(s: &FixtureStatus) -> FixtureStatus {
    s.clone()
}
fn to_demo_fixture(f: FixtureSnapshot) -> DemoFixture {
    DemoFixture {
        action_id: f.action_id,
        receipt_id: f.receipt_id,
        status: map_fixture_status(&f.status),
        bytes: f.bytes,
        sha256: f.sha256,
        identity_sha256: f.identity_sha256,
    }
}

// Every test drives DemoExecutionService::open, which acquires the owner
// lease — a Unix-only mechanism by design (non-unix returns
// SessionUnavailable). Gate the whole module the same way acquire_lease is
// gated, so cross-platform `cargo test --workspace` exercises exactly the
// contracts that are portable.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    };
    use tempfile::tempdir;

    fn fixture_service() -> (tempfile::TempDir, DemoExecutionService, PlanView) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().expect("owned test root");
        // Mirror the production contract: the app-owned root is private
        // (0700) so the purge control store's private-parent check passes.
        let mut permissions = std::fs::metadata(dir.path())
            .expect("root metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(dir.path(), permissions).expect("root permissions");
        let mut service = DemoExecutionService::open(dir.path()).expect("open service");
        service.start_demo().expect("create disposable fixture");
        let plan = service.plan.get_plan().expect("plan");
        (dir, service, plan)
    }

    #[test]
    fn status_reports_needs_inspection_for_permanently_removed_fixtures() {
        let (dir, mut service, _plan) = fixture_service();
        // Fabricate a fully-journaled permanent removal directly in the
        // lifecycle store (the coordinator purge flow arrives in a later
        // batch): a stage row, a purge terminal row, and the terminal fixture
        // state. The demo state machine must not call such a session "ready".
        let fixture_root = std::fs::read_dir(dir.path())
            .unwrap()
            .find_map(|e| {
                let name = e.unwrap().file_name().into_string().unwrap();
                name.starts_with(".disk-steward-fixtures-")
                    .then(|| dir.path().join(name))
            })
            .unwrap();
        let db = Connection::open(fixture_root.join("lifecycle.sqlite3")).unwrap();
        let action: String = db
            .query_row(
                "SELECT action_id FROM fixtures ORDER BY rowid LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let receipt = wire(&disk_steward_core::ReceiptId::new()).unwrap();
        db.execute(
            "INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'stage',?3,'staged')",
            params![wire(&AttemptId::new()).unwrap(), action, receipt],
        )
        .unwrap();
        db.execute(
            "INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'purge',?3,'permanently_removed')",
            params![wire(&AttemptId::new()).unwrap(), action, receipt],
        )
        .unwrap();
        db.execute(
            "UPDATE fixtures SET status='permanently_removed', receipt_id=?2 WHERE action_id=?1",
            params![action, receipt],
        )
        .unwrap();
        drop(db);
        let status = service.status().unwrap();
        assert_eq!(
            status.state, "needs_inspection",
            "a permanently removed fixture must not report the session ready"
        );
        assert!(status.fixtures.iter().all(|f| f.status
            != disk_steward_fixture_lifecycle::FixtureStatus::Available
            || f.action_id != serde_json::from_str(&action).unwrap()));
    }

    #[test]
    fn status_refuses_to_reopen_fixtures_without_owner_lease() {
        // The fixture-runtime-schema-v2 contract requires the owner lease to
        // be held for the complete reopen, because reopen may now perform the
        // v1->v2 write migration. After a poison released the lease, status()
        // must refuse to lazily reopen the fixtures instead of migrating
        // leaseless. (With the lifecycle handle still held, status() remains
        // usable after a poison so the UI can show the uncertain state.)
        let (_dir, mut service, _plan) = fixture_service();
        service.poisoned = true;
        service.lease.take();
        service.lifecycle = None;
        assert_eq!(
            service.status().unwrap_err(),
            DemoError::NeedsInspection,
            "leaseless reopen must be refused, not performed"
        );
    }

    fn include_one(service: &mut DemoExecutionService, plan: &PlanView) -> PlanView {
        service
            .plan
            .edit(
                plan.revision_id,
                plan.actions[0].id,
                disk_steward_review_plan::Decision::Include,
            )
            .expect("include first fixture")
    }

    #[test]
    fn stage_restore_and_reopen_report_disposable_outcomes() {
        let (dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let review = service
            .prepare_stage(plan.revision_id)
            .expect("stage review");
        let staged = service
            .confirm_and_execute(review.review_id, |_| true)
            .expect("stage");
        assert_eq!(staged.state, "finished");
        assert_eq!(staged.staged_bytes, review.selected_bytes);
        assert_eq!(staged.recovered_bytes, 0);
        let receipt = staged.items[0].receipt_id.expect("stage receipt");
        let restore = service
            .prepare_restore(plan.revision_id, receipt)
            .expect("restore review");
        let restored = service
            .confirm_and_execute(restore.review_id, |_| true)
            .expect("restore");
        assert_eq!(restored.state, "finished");
        assert_eq!(restored.restored_bytes, restore.selected_bytes);
        drop(service);
        let reopened = DemoExecutionService::open(dir.path()).expect("reopen");
        assert_eq!(
            reopened
                .ledger
                .execution_intent(&wire(&restored.execution_id).expect("id"))
                .expect("ledger intent")
                .state,
            disk_steward_approval_ledger::IntentState::Finished
        );
    }

    #[test]
    fn cancellation_does_not_mutate_and_review_is_single_use() {
        let (_dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let review = service.prepare_stage(plan.revision_id).expect("review");
        assert_eq!(
            service
                .confirm_and_execute(review.review_id, |_| false)
                .unwrap_err(),
            DemoError::ConfirmationCancelled
        );
        assert_eq!(
            service.status().expect("unchanged status").fixtures[0].status,
            FixtureStatus::Available
        );
        let result = service
            .confirm_and_execute(review.review_id, |_| true)
            .expect("first acceptance");
        assert_eq!(result.state, "finished");
        assert!(service
            .confirm_and_execute(review.review_id, |_| true)
            .is_err());
    }

    #[test]
    fn restore_collision_is_partial_and_preserves_both_receipt_and_sentinel() {
        let (dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let stage = service
            .prepare_stage(plan.revision_id)
            .expect("stage review");
        let staged = service
            .confirm_and_execute(stage.review_id, |_| true)
            .expect("stage");
        let receipt = staged.items[0].receipt_id.expect("receipt");
        let session = staged.items[0].action_id;
        let fixture = service.snapshot_for_item(session).expect("staged fixture");
        assert_eq!(fixture.status, FixtureStatus::Staged);
        let session_id = service.lifecycle.as_ref().expect("lifecycle").session_id();
        let folder = wire(&session_id)
            .expect("session")
            .trim_matches('"')
            .rsplit(':')
            .next()
            .unwrap()
            .to_owned();
        let slot = dir.path().join(format!(
            ".disk-steward-fixtures-{folder}/source/compiler-output"
        ));
        fs::write(&slot, b"sentinel must survive restore collision")
            .expect("create collision sentinel");
        let restore = service
            .prepare_restore(plan.revision_id, receipt)
            .expect("restore review");
        let result = service
            .confirm_and_execute(restore.review_id, |_| true)
            .expect("known partial result");
        assert_eq!(result.state, "partial");
        assert_eq!(result.items[0].status, "collision");
        assert_eq!(result.restored_bytes, 0);
        assert_eq!(result.recovered_bytes, 0);
        assert_eq!(
            fs::read(&slot).expect("sentinel preserved"),
            b"sentinel must survive restore collision"
        );
        assert_eq!(
            service
                .snapshot_for_item(session)
                .expect("receipt retained")
                .receipt_id,
            Some(receipt)
        );
    }

    #[test]
    fn stale_plan_and_changed_content_during_confirmation_are_refused() {
        let (dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let review = service.prepare_stage(plan.revision_id).expect("review");
        let path = dir.path().join(PLAN_DB);
        let stale = service.confirm_and_execute(review.review_id, move |_| {
            let mut other = ReviewPlanStore::open(path).expect("second plan handle");
            let current = other.get_plan().expect("current");
            other
                .edit(
                    current.revision_id,
                    current.actions[1].id,
                    disk_steward_review_plan::Decision::Include,
                )
                .expect("edit");
            true
        });
        assert_eq!(stale.unwrap_err(), DemoError::StaleRevision);
        assert_eq!(
            service.status().expect("still available").fixtures[0].status,
            FixtureStatus::Available
        );

        let plan = service.plan.get_plan().expect("new plan");
        let review = service.prepare_stage(plan.revision_id).expect("new review");
        let component = wire(&review.session_id)
            .expect("session")
            .trim_matches('"')
            .rsplit(':')
            .next()
            .unwrap()
            .to_owned();
        let file = dir.path().join(format!(
            ".disk-steward-fixtures-{component}/source/compiler-output"
        ));
        let changed = service.confirm_and_execute(review.review_id, move |_| {
            fs::write(file, b"changed fixture bytes").expect("tamper test fixture");
            true
        });
        assert_eq!(changed.unwrap_err(), DemoError::FixtureChanged);
    }

    #[test]
    fn self_consistent_manifest_tampering_cannot_select_a_kept_action() {
        let (_dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let review = service.prepare_stage(plan.revision_id).expect("review");
        let mut manifest: FileManifest = service
            .control
            .query_row(
                "SELECT manifest_json FROM prepared_reviews WHERE review_id=?1",
                [wire(&review.review_id).unwrap()],
                |row| {
                    serde_json::from_str(&row.get::<_, String>(0)?)
                        .map_err(|_| rusqlite::Error::InvalidQuery)
                },
            )
            .expect("stored manifest");
        manifest.selected = vec![plan.actions[1].id];
        let tampered_json = bounded_json(&manifest).expect("bounded tampered manifest");
        let tampered_hash = hash_json(&manifest).expect("matching hash");
        service.control.execute(
            "UPDATE prepared_reviews SET manifest_json=?1,manifest_sha256=?2 WHERE review_id=?3",
            params![tampered_json,tampered_hash,wire(&review.review_id).unwrap()],
        ).expect("simulate internally corrupted row");
        let mut callback_called = false;
        let result = service.confirm_and_execute(review.review_id, |_| {
            callback_called = true;
            true
        });
        assert_eq!(result.unwrap_err(), DemoError::InvalidReview);
        assert!(
            !callback_called,
            "invalid stored authority must be rejected before prompting"
        );
        assert_eq!(
            service.status().expect("no file mutation").fixtures[0].status,
            FixtureStatus::Available
        );
    }

    #[test]
    fn expiry_wall_clock_reversal_and_creation_intent_fail_closed() {
        let dir = tempdir().expect("root");
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(dir.path())
            .expect("root metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(dir.path(), permissions).expect("root permissions");
        let clock = Arc::new(AtomicI64::new(1_000));
        let read = clock.clone();
        let mut service =
            DemoExecutionService::open_with_clock(dir.path(), move || read.load(Ordering::SeqCst))
                .expect("open");
        service.start_demo().expect("start");
        let plan = service.plan.get_plan().expect("plan");
        let plan = include_one(&mut service, &plan);
        let review = service.prepare_stage(plan.revision_id).expect("review");
        clock.store(review.expires_at_ms, Ordering::SeqCst);
        assert_eq!(
            service
                .confirm_and_execute(review.review_id, |_| true)
                .unwrap_err(),
            DemoError::ReviewExpired
        );
        clock.store(500, Ordering::SeqCst);
        assert_eq!(
            service.prepare_stage(plan.revision_id).unwrap_err(),
            DemoError::ClockReversed
        );

        drop(service);
        let control = Connection::open(dir.path().join(EXEC_DB)).expect("control db");
        control
            .execute(
                "UPDATE control SET creation_state='started' WHERE singleton=1",
                [],
            )
            .expect("simulate interrupted creation intent");
        drop(control);
        let mut reopened = DemoExecutionService::open(dir.path()).expect("reopen");
        assert_eq!(reopened.status().expect("status").state, "needs_inspection");
        assert_eq!(
            reopened.start_demo().unwrap_err(),
            DemoError::NeedsInspection
        );
    }

    #[test]
    fn editing_only_the_expiry_column_is_detected_before_confirmation() {
        let (dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let review = service.prepare_stage(plan.revision_id).expect("review");
        let changed = service
            .control
            .execute(
                "UPDATE prepared_reviews SET expires_at_ms=?1 WHERE review_id=?2",
                params![
                    review.expires_at_ms + REVIEW_TTL_MS,
                    wire(&review.review_id).unwrap()
                ],
            )
            .expect("tamper only the unhashed column");
        assert_eq!(changed, 1);
        let mut callback_called = false;
        assert_eq!(
            service
                .confirm_and_execute(review.review_id, |_| {
                    callback_called = true;
                    true
                })
                .unwrap_err(),
            DemoError::CorruptData
        );
        assert!(!callback_called);
        assert_eq!(
            service.status().expect("fixture unchanged").fixtures[0].status,
            FixtureStatus::Available
        );
        drop(dir);
    }

    #[test]
    fn overlong_lifetime_is_rejected_even_with_a_recomputed_manifest_hash() {
        let (_dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let review = service.prepare_stage(plan.revision_id).expect("review");
        let review_wire = wire(&review.review_id).unwrap();
        let json: String = service
            .control
            .query_row(
                "SELECT manifest_json FROM prepared_reviews WHERE review_id=?1",
                [&review_wire],
                |row| row.get(0),
            )
            .expect("stored manifest");
        let mut manifest: FileManifest = serde_json::from_str(&json).expect("manifest");
        manifest.expires_at_ms += 1;
        let tampered_json = bounded_json(&manifest).expect("tampered manifest");
        let tampered_hash = hash_json(&manifest).expect("self-consistent hash");
        service.control.execute(
            "UPDATE prepared_reviews SET manifest_json=?1,manifest_sha256=?2,expires_at_ms=?3 WHERE review_id=?4",
            params![tampered_json, tampered_hash, manifest.expires_at_ms, review_wire],
        ).expect("persist internally consistent overlong review");
        assert_eq!(
            service
                .confirm_and_execute(review.review_id, |_| true)
                .unwrap_err(),
            DemoError::CorruptData
        );
        assert_eq!(
            service.status().expect("fixture unchanged").fixtures[0].status,
            FixtureStatus::Available
        );
    }

    #[test]
    fn interrupted_started_batch_is_reported_uncertain_without_resume() {
        let (dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let execution = ExecutionId::new();
        let approval = ApprovalId::new();
        let attempt = AttemptId::new();
        let review = ReviewId::new();
        let now = wall_now_ms();
        let binding = BatchBinding {
            execution_id: execution,
            approval_id: approval,
            attempts: vec![attempt],
            review_id: review,
            digest: "a".repeat(64),
        };
        let tx = service
            .control
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("control transaction");
        tx.execute(
            "UPDATE control SET active_execution_id=?1 WHERE singleton=1",
            [wire(&execution).expect("execution ID")],
        )
        .expect("claim");
        tx.execute("INSERT INTO executions(execution_id,review_id,approval_id,operation,state,batch_json,started_at_ms,finished_at_ms) VALUES(?1,?2,?3,'stage','started',?4,?5,NULL)", params![wire(&execution).unwrap(),wire(&review).unwrap(),wire(&approval).unwrap(),bounded_json(&binding).unwrap(),now]).expect("execution record");
        tx.execute("INSERT INTO child_attempts(attempt_id,execution_id,action_id,state,receipt_id) VALUES(?1,?2,?3,'pending',NULL)", params![wire(&attempt).unwrap(),wire(&execution).unwrap(),wire(&plan.actions[0].id).unwrap()]).expect("child record");
        tx.commit().expect("commit intent");
        let digest = "a".repeat(64);
        service
            .ledger
            .insert_approval(&wire(&approval).unwrap(), &digest, now + REVIEW_TTL_MS)
            .expect("approval");
        service
            .ledger
            .consume_approval(&wire(&approval).unwrap(), &digest, now)
            .expect("consume");
        service
            .ledger
            .record_intent_started(&wire(&execution).unwrap(), &wire(&approval).unwrap(), now)
            .expect("ledger intent");
        assert!(matches!(
            DemoExecutionService::open(dir.path()),
            Err(DemoError::Busy)
        ));
        let still_started: String = service
            .control
            .query_row(
                "SELECT state FROM executions WHERE execution_id=?1",
                [wire(&execution).unwrap()],
                |row| row.get(0),
            )
            .expect("live job left untouched");
        assert_eq!(still_started, "started");
        assert_eq!(
            service
                .ledger
                .execution_intent(&wire(&execution).unwrap())
                .expect("live ledger untouched")
                .state,
            disk_steward_approval_ledger::IntentState::Started
        );
        drop(service);

        let mut reopened = DemoExecutionService::open(dir.path()).expect("reopen");
        let status = reopened.status().expect("recovered status");
        let last = status.last_execution.expect("uncertain execution shown");
        assert_eq!(status.state, "needs_inspection");
        assert_eq!(last.state, "uncertain");
        assert_eq!(last.operation, "stage");
        assert_eq!(last.items[0].action_id, plan.actions[0].id);
        assert_eq!(last.items[0].status, "uncertain");
        assert_eq!(status.fixtures[0].status, FixtureStatus::Available);
        assert_eq!(
            reopened.prepare_stage(plan.revision_id).unwrap_err(),
            DemoError::NeedsInspection
        );
        assert_eq!(
            reopened
                .ledger
                .execution_intent(&wire(&execution).unwrap())
                .expect("ledger terminal")
                .state,
            disk_steward_approval_ledger::IntentState::Uncertain
        );
        assert_eq!(
            reopened
                .start_demo()
                .expect("idempotent existing session")
                .session_id,
            status.session_id
        );
    }

    #[test]
    fn interrupted_batch_recovery_counts_durable_completed_items() {
        let (dir, mut service, initial) = fixture_service();
        let first = service
            .plan
            .edit(
                initial.revision_id,
                initial.actions[0].id,
                disk_steward_review_plan::Decision::Include,
            )
            .expect("include first fixture");
        let plan = service
            .plan
            .edit(
                first.revision_id,
                initial.actions[1].id,
                disk_steward_review_plan::Decision::Include,
            )
            .expect("include second fixture");
        let review = service
            .prepare_stage(plan.revision_id)
            .expect("two-item review");
        let execution = ExecutionId::new();
        let approval = ApprovalId::new();
        let attempts = vec![AttemptId::new(), AttemptId::new()];
        let receipt = service
            .lifecycle
            .as_mut()
            .expect("fixture lifecycle")
            .stage(review.items[0].action_id, attempts[0])
            .expect("first fixture moved before simulated crash");
        let binding = BatchBinding {
            execution_id: execution,
            approval_id: approval,
            attempts: attempts.clone(),
            review_id: review.review_id,
            digest: review.manifest_sha256.clone(),
        };
        let now = wall_now_ms();
        let tx = service
            .control
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("control transaction");
        tx.execute(
            "UPDATE control SET active_execution_id=?1 WHERE singleton=1",
            [wire(&execution).unwrap()],
        )
        .expect("claim active batch");
        tx.execute(
            "UPDATE prepared_reviews SET claimed=1 WHERE review_id=?1",
            [wire(&review.review_id).unwrap()],
        )
        .expect("claim review");
        tx.execute("INSERT INTO executions(execution_id,review_id,approval_id,operation,state,batch_json,started_at_ms,finished_at_ms) VALUES(?1,?2,?3,'stage','started',?4,?5,NULL)", params![wire(&execution).unwrap(),wire(&review.review_id).unwrap(),wire(&approval).unwrap(),bounded_json(&binding).unwrap(),now]).expect("execution intent");
        tx.execute(
            "INSERT INTO child_attempts(attempt_id,execution_id,action_id,state,receipt_id) VALUES(?1,?2,?3,'staged',?4)",
            params![wire(&attempts[0]).unwrap(),wire(&execution).unwrap(),wire(&review.items[0].action_id).unwrap(),wire(&receipt).unwrap()],
        ).expect("persist known first item");
        tx.execute(
            "INSERT INTO child_attempts(attempt_id,execution_id,action_id,state,receipt_id) VALUES(?1,?2,?3,'pending',NULL)",
            params![wire(&attempts[1]).unwrap(),wire(&execution).unwrap(),wire(&review.items[1].action_id).unwrap()],
        ).expect("persist pending second item");
        tx.commit().expect("persist simulated crash point");
        drop(service);

        let mut reopened = DemoExecutionService::open(dir.path()).expect("recover");
        let recovered = reopened
            .status()
            .expect("status")
            .last_execution
            .expect("recovered execution");
        assert_eq!(recovered.state, "uncertain");
        assert_eq!(recovered.staged_bytes, review.items[0].bytes);
        assert_eq!(recovered.restored_bytes, 0);
        assert_eq!(recovered.items[0].status, "staged");
        assert_eq!(recovered.items[0].receipt_id, Some(receipt));
        assert_eq!(recovered.items[1].status, "uncertain");
        assert_eq!(recovered.items[1].receipt_id, None);
    }

    #[test]
    fn ledger_failure_after_claim_marks_uncertain_and_releases_execution_lock() {
        let (dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let review = service.prepare_stage(plan.revision_id).expect("review");
        let ledger_path = dir.path().join(LEDGER_DB);
        let result = service.confirm_and_execute(review.review_id, move |_| {
            let external = Connection::open(ledger_path).expect("ledger connection");
            external
                .execute("DROP TABLE approvals", [])
                .expect("force ledger insert failure");
            true
        });
        assert_eq!(result.unwrap_err(), DemoError::LedgerUnavailable);
        let (state, active): (String, Option<String>) = service.control.query_row(
            "SELECT e.state,c.active_execution_id FROM executions e CROSS JOIN control c ORDER BY e.started_at_ms DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).expect("claim settled");
        assert_eq!(state, "uncertain");
        assert_eq!(active, None);
        assert_eq!(
            service.status().expect("no file call occurred").fixtures[0].status,
            FixtureStatus::Available
        );
    }

    #[test]
    fn purge_through_coordinator_unlinks_and_consumes_the_approval() {
        let (dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let revision = plan.revision_id;
        // Stage fixture 0 through the demo flow.
        let stage_review = service.prepare_stage(revision).expect("stage review");
        service
            .confirm_and_execute(stage_review.review_id, |_| true)
            .expect("stage confirm");
        // Read the staged receipt and its action from the lifecycle store.
        let (action, receipt): (String, String) = {
            let entry = std::fs::read_dir(dir.path())
                .unwrap()
                .find_map(|e| {
                    let name = e.unwrap().file_name().into_string().unwrap();
                    name.starts_with(".disk-steward-fixtures-").then_some(name)
                })
                .unwrap();
            let db = Connection::open(dir.path().join(entry).join("lifecycle.sqlite3")).unwrap();
            db.query_row(
                "SELECT action_id, receipt_id FROM fixtures WHERE status='staged'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        let targets = vec![disk_steward_fixture_purge::PurgeTarget {
            action_id: serde_json::from_str(&action).unwrap(),
            receipt_id: serde_json::from_str(&receipt).unwrap(),
        }];
        // Prepare and confirm the purge through the coordinator.
        let purge_review = service
            .prepare_purge_review(revision, targets)
            .expect("purge review");
        let result = service
            .confirm_purge_execution(purge_review.review_id, |_| true)
            .expect("purge executed");
        assert_eq!(result.state, "finished");
        assert_eq!(result.items.len(), 1);
        assert!(matches!(
            result.items[0].outcome,
            disk_steward_fixture_lifecycle::PurgeItemOutcome::PermanentlyRemoved { .. }
        ));
        // The demo now honestly reports the permanently removed fixture.
        let status = service.status().expect("status");
        assert_eq!(status.fixtures[0].status, FixtureStatus::PermanentlyRemoved);
        assert_eq!(status.state, "needs_inspection");
        // The approval was consumed once through the existing ledger: two
        // approvals total (the demo stage review plus this purge), each
        // consumed exactly once.
        let db = Connection::open(dir.path().join(LEDGER_DB)).unwrap();
        let approvals: i64 = db
            .query_row("SELECT COUNT(*) FROM approvals", [], |r| r.get(0))
            .unwrap();
        assert_eq!(approvals, 2, "the stage review plus this purge review");
        let consumed: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM approvals WHERE consumed_at_unix_ms IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(consumed, 2, "both approvals are consumed exactly once");
        let intents: i64 = db
            .query_row("SELECT COUNT(*) FROM execution_intents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            intents, 2,
            "the stage and purge execution intents are durably recorded"
        );
    }

    #[test]
    fn purge_ledger_failure_marks_uncertain_and_blocks_new_work() {
        let (dir, mut service, plan) = fixture_service();
        let plan = include_one(&mut service, &plan);
        let revision = plan.revision_id;
        let stage_review = service.prepare_stage(revision).expect("stage review");
        service
            .confirm_and_execute(stage_review.review_id, |_| true)
            .expect("stage confirm");
        let (action, receipt): (String, String) = {
            let entry = std::fs::read_dir(dir.path())
                .unwrap()
                .find_map(|e| {
                    let name = e.unwrap().file_name().into_string().unwrap();
                    name.starts_with(".disk-steward-fixtures-").then_some(name)
                })
                .unwrap();
            let db = Connection::open(dir.path().join(entry).join("lifecycle.sqlite3")).unwrap();
            db.query_row(
                "SELECT action_id, receipt_id FROM fixtures WHERE status='staged'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        let targets = vec![disk_steward_fixture_purge::PurgeTarget {
            action_id: serde_json::from_str(&action).unwrap(),
            receipt_id: serde_json::from_str(&receipt).unwrap(),
        }];
        let purge_review = service
            .prepare_purge_review(revision, targets)
            .expect("purge review");
        // Force the ledger failure inside the confirmation callback.
        let ledger_path = dir.path().join(LEDGER_DB);
        let error = service
            .confirm_purge_execution(purge_review.review_id, move |_| {
                // Force the ledger failure: drop the intents reference first,
                // then the approvals table itself.
                let external = Connection::open(&ledger_path).expect("ledger connection");
                external
                    .execute("DROP TABLE execution_intents", [])
                    .expect("force ledger failure (intents)");
                external
                    .execute("DROP TABLE approvals", [])
                    .expect("force ledger failure (approvals)");
                true
            })
            .unwrap_err();
        assert_eq!(error.code(), "ledger_unavailable");
        // Cross-store uncertainty: new stage/restore work and further purge
        // reviews are blocked until inspection.
        let next = service.prepare_stage(revision);
        assert_eq!(next.unwrap_err(), DemoError::NeedsInspection);
        let second = service.prepare_purge_review(revision, Vec::new());
        assert_eq!(second.unwrap_err(), DemoError::NeedsInspection);
        let status = service.status().expect("status");
        assert_eq!(status.state, "needs_inspection");
        // The staged fixture itself was never unlinked and its lifecycle row
        // stays 'staged': the failure was in the authority layer (before the
        // lifecycle ever saw a purge intent). The uncertainty lives in the
        // control stores and blocks new work via the consults above.
        let fixture_status: String = {
            let entry = std::fs::read_dir(dir.path())
                .unwrap()
                .find_map(|e| {
                    let name = e.unwrap().file_name().into_string().unwrap();
                    name.starts_with(".disk-steward-fixtures-").then_some(name)
                })
                .unwrap();
            let db = Connection::open(dir.path().join(entry).join("lifecycle.sqlite3")).unwrap();
            db.query_row(
                "SELECT status FROM fixtures WHERE status IN ('staged','uncertain','permanently_removed')",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(fixture_status, "staged");
    }

    #[test]
    fn lifecycle_uncertain_stops_batch_and_leaves_later_fixture_untouched() {
        let (dir, mut service, initial) = fixture_service();
        let first_included = service
            .plan
            .edit(
                initial.revision_id,
                initial.actions[0].id,
                disk_steward_review_plan::Decision::Include,
            )
            .expect("include first fixture");
        let plan = service
            .plan
            .edit(
                first_included.revision_id,
                initial.actions[1].id,
                disk_steward_review_plan::Decision::Include,
            )
            .expect("include second fixture");
        let review = service
            .prepare_stage(plan.revision_id)
            .expect("two-item review");
        assert_eq!(review.items.len(), 2);

        let execution = ExecutionId::new();
        let approval = ApprovalId::new();
        let review_attempts = vec![AttemptId::new(), AttemptId::new()];
        let binding = BatchBinding {
            execution_id: execution,
            approval_id: approval,
            attempts: review_attempts.clone(),
            review_id: review.review_id,
            digest: review.manifest_sha256.clone(),
        };
        let now = wall_now_ms();
        let tx = service
            .control
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("control transaction");
        tx.execute(
            "UPDATE control SET active_execution_id=?1 WHERE singleton=1",
            [wire(&execution).unwrap()],
        )
        .expect("claim active batch");
        tx.execute(
            "UPDATE prepared_reviews SET claimed=1 WHERE review_id=?1",
            [wire(&review.review_id).unwrap()],
        )
        .expect("claim review");
        tx.execute("INSERT INTO executions(execution_id,review_id,approval_id,operation,state,batch_json,started_at_ms,finished_at_ms) VALUES(?1,?2,?3,'stage','started',?4,?5,NULL)", params![wire(&execution).unwrap(),wire(&review.review_id).unwrap(),wire(&approval).unwrap(),bounded_json(&binding).unwrap(),now]).expect("batch record");
        for (item, attempt) in review.items.iter().zip(&review_attempts) {
            tx.execute("INSERT INTO child_attempts(attempt_id,execution_id,action_id,state,receipt_id) VALUES(?1,?2,?3,'pending',NULL)", params![wire(attempt).unwrap(),wire(&execution).unwrap(),wire(&item.action_id).unwrap()]).expect("child attempt");
        }
        tx.commit().expect("commit batch intent");
        let ledger_digest = hex(&sha_bytes(review.manifest_sha256.as_bytes()));
        service
            .ledger
            .insert_approval(
                &wire(&approval).unwrap(),
                &ledger_digest,
                now + REVIEW_TTL_MS,
            )
            .expect("ledger approval");
        service
            .ledger
            .consume_approval(&wire(&approval).unwrap(), &ledger_digest, now)
            .expect("consume approval");
        service
            .ledger
            .record_intent_started(&wire(&execution).unwrap(), &wire(&approval).unwrap(), now)
            .expect("ledger intent");

        let session = service.lifecycle.as_ref().expect("lifecycle").session_id();
        let component = wire(&session)
            .unwrap()
            .trim_matches('"')
            .rsplit(':')
            .next()
            .unwrap()
            .to_owned();
        let first_source = dir.path().join(format!(
            ".disk-steward-fixtures-{component}/source/compiler-output"
        ));
        fs::write(&first_source, b"changed after the review was prepared")
            .expect("change first fixture identity");

        // Call the private execution stage after the service has durably claimed
        // the review and ledger intent. FixtureLifecycle reports the changed
        // first item uncertain, so the second item must not reach stage().
        let result = service
            .execute_batch(&review, execution, &review_attempts, now)
            .expect("uncertain result persisted");
        assert_eq!(result.state, "uncertain");
        assert_eq!(result.items[0].status, "uncertain");
        assert_eq!(result.items[1].status, "not_attempted");
        let status = service.status().expect("status remains inspectable");
        assert_eq!(status.state, "needs_inspection");
        assert_eq!(status.fixtures[1].status, FixtureStatus::Available);
        let second_state: String = service
            .control
            .query_row(
                "SELECT state FROM child_attempts WHERE attempt_id=?1",
                [wire(&review_attempts[1]).unwrap()],
                |row| row.get(0),
            )
            .expect("blocked child state");
        assert_eq!(second_state, "uncertain");
    }

    #[test]
    fn malformed_recovery_binding_fails_closed_without_mutation() {
        let (dir, service, _) = fixture_service();
        let session = service.lifecycle.as_ref().expect("lifecycle").session_id();
        let execution = ExecutionId::new();
        let wrong_execution = ExecutionId::new();
        let review = ReviewId::new();
        let approval = ApprovalId::new();
        let attempt = AttemptId::new();
        let action = service.plan.get_plan().expect("plan").actions[0].id;
        let binding = BatchBinding {
            execution_id: wrong_execution,
            approval_id: approval,
            attempts: vec![attempt],
            review_id: review,
            digest: "c".repeat(64),
        };
        let tx = service
            .control
            .unchecked_transaction()
            .expect("recovery transaction");
        tx.execute(
            "UPDATE control SET active_execution_id=?1 WHERE singleton=1",
            [wire(&execution).unwrap()],
        )
        .expect("active execution");
        tx.execute("INSERT INTO executions(execution_id,review_id,approval_id,operation,state,batch_json,started_at_ms,finished_at_ms) VALUES(?1,?2,?3,'stage','started',?4,?5,NULL)", params![wire(&execution).unwrap(),wire(&review).unwrap(),wire(&approval).unwrap(),bounded_json(&binding).unwrap(),wall_now_ms()]).expect("corrupt binding row");
        tx.execute("INSERT INTO child_attempts(attempt_id,execution_id,action_id,state,receipt_id) VALUES(?1,?2,?3,'pending',NULL)", params![wire(&attempt).unwrap(),wire(&execution).unwrap(),wire(&action).unwrap()]).expect("pending child");
        tx.commit().expect("persist malformed state");
        let before: (String, String) = service
            .control
            .query_row(
                "SELECT state,batch_json FROM executions WHERE execution_id=?1",
                [wire(&execution).unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("before state");
        drop(service);

        assert!(matches!(
            DemoExecutionService::open(dir.path()),
            Err(DemoError::CorruptData)
        ));
        let control =
            Connection::open(dir.path().join(EXEC_DB)).expect("control db still readable");
        let after: (String, String) = control
            .query_row(
                "SELECT state,batch_json FROM executions WHERE execution_id=?1",
                [wire(&execution).unwrap()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("after state");
        assert_eq!(after, before);
        let (_, snapshot) =
            FixtureLifecycle::reopen(dir.path(), session).expect("fixtures still inspectable");
        assert!(snapshot
            .fixtures
            .iter()
            .all(|fixture| fixture.status == FixtureStatus::Available));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_lifetime_lock_is_rejected_before_database_recovery() {
        use std::os::unix::fs::symlink;
        let dir = tempdir().expect("root");
        let target = dir.path().join("target");
        fs::write(&target, b"sentinel").expect("target");
        symlink(&target, dir.path().join(".demo-execution-v1.lock")).expect("symlink");
        assert!(matches!(
            DemoExecutionService::open(dir.path()),
            Err(DemoError::Storage)
        ));
        assert!(!dir.path().join(EXEC_DB).exists());
    }
}
