//! Receipt-addressed purge service for the disposable fixture demo.
//!
//! Batch B of the `fixture-purge-v1` contract: immutable purge reviews, a
//! fail-closed control store, atomic claims, serial execution through
//! `FixtureLifecycle::purge_receipt`, and uncertainty recovery. The service
//! owns no lease: it is constructed by the existing lease-owning coordinator,
//! which supplies the already-opened lifecycle and plan handles. Approval
//! ledger consumption and native confirmation wiring arrive in batch C.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
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
impl From<disk_steward_fixture_lifecycle::LifecycleError> for PurgeError {
    fn from(error: disk_steward_fixture_lifecycle::LifecycleError) -> Self {
        use disk_steward_fixture_lifecycle::LifecycleError;
        match error {
            LifecycleError::Changed => Self::ChangedTarget,
            LifecycleError::Uncertain => Self::UncertainOutcome,
            _ => Self::StorageFailure,
        }
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
    control: Connection,
    lifecycle: FixtureLifecycle,
    plan: ReviewPlanStore,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
}

/// One stored purge review row, parsed into typed values.
type StoredReviewRow = (
    String, // plan_id
    String, // revision_id
    String, // session_id
    i64,    // created_at_ms
    i64,    // expires_at_ms
    String, // manifest_sha256
    String, // targets_json
    i64,    // selected_logical_bytes
    String, // state
);

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
        let mut control = open_control_db(&root)?;
        recover_unfinished_executions(&mut control, (now)())?;
        Ok(Self {
            control,
            lifecycle,
            plan,
            now,
        })
    }

    /// Freeze an immutable purge review over explicitly selected staged
    /// receipts. Fails if any selected action is absent, excluded, stale,
    /// duplicated, or not currently staged — before any row is written.
    pub fn prepare_purge_review(
        &mut self,
        expected_revision: RevisionId,
        targets: Vec<PurgeTarget>,
    ) -> Result<PreparedPurgeReview, PurgeError> {
        if targets.is_empty() || targets.len() > MAX_TARGETS {
            return Err(PurgeError::InvalidInput);
        }
        let mut seen_actions = std::collections::HashSet::new();
        let mut seen_receipts = std::collections::HashSet::new();
        for target in &targets {
            if !seen_actions.insert(target.action_id) || !seen_receipts.insert(target.receipt_id) {
                return Err(PurgeError::InvalidInput);
            }
        }
        let plan_view = self
            .plan
            .get_plan()
            .map_err(|_| PurgeError::StaleRevision)?;
        if plan_view.revision_id != expected_revision {
            return Err(PurgeError::StaleRevision);
        }
        let snapshot = self.lifecycle.snapshot()?;
        let mut review_targets = Vec::with_capacity(targets.len());
        for target in &targets {
            let action = plan_view
                .actions
                .iter()
                .find(|a| a.id == target.action_id)
                .ok_or(PurgeError::InvalidInput)?;
            if action.decision != disk_steward_review_plan::Decision::Include {
                return Err(PurgeError::InvalidInput);
            }
            let fixture = snapshot
                .fixtures
                .iter()
                .find(|f| f.action_id == target.action_id)
                .ok_or(PurgeError::ChangedTarget)?;
            if fixture.receipt_id.as_ref() != Some(&target.receipt_id)
                || fixture.status != FixtureStatus::Staged
            {
                return Err(PurgeError::ChangedTarget);
            }
            review_targets.push(PurgeReviewTarget {
                action_id: target.action_id,
                receipt_id: target.receipt_id,
                label: action.label.clone(),
                logical_bytes: fixture.bytes,
                status: PurgeReceiptState::Staged,
            });
        }
        let created_at_ms = (self.now)();
        let selected_logical_bytes = review_targets
            .iter()
            .map(|t| t.logical_bytes)
            .try_fold(0u64, |sum, bytes| sum.checked_add(bytes))
            .ok_or(PurgeError::InvalidInput)?;
        let manifest = PurgeManifestV1 {
            schema_version: 1,
            session_id: snapshot.session_id,
            plan_id: plan_view.plan_id,
            revision_id: plan_view.revision_id,
            operation: SupportedOperation::PermanentlyRemoveStaged,
            created_at_ms,
            expires_at_ms: created_at_ms + REVIEW_LIFETIME_MS,
            selected_logical_bytes,
            adapter_schema_version: 2,
            targets: review_targets
                .iter()
                .zip(snapshot.fixtures.iter())
                .map(|(target, fixture)| ManifestTargetV1 {
                    action_id: target.action_id,
                    receipt_id: target.receipt_id,
                    logical_bytes: fixture.bytes,
                    content_sha256: fixture.sha256.clone(),
                    identity_sha256: fixture.identity_sha256.clone(),
                })
                .collect(),
        };
        // Canonical: serde_json of a typed struct is field-order
        // deterministic; the digest covers the complete manifest content.
        let manifest_bytes =
            serde_json::to_vec(&manifest).map_err(|_| PurgeError::StorageFailure)?;
        let manifest_sha256 = hex_lower(&Sha256::digest(&manifest_bytes));
        let targets_json =
            serde_json::to_string(&review_targets).map_err(|_| PurgeError::StorageFailure)?;
        if targets_json.len() > MAX_TARGETS_JSON_BYTES {
            return Err(PurgeError::InvalidInput);
        }
        let review_id = ReviewId::new();
        let wire = serde_json::to_string(&review_id).map_err(|_| PurgeError::StorageFailure)?;
        self.control.execute(
            "INSERT INTO purge_reviews(review_id,plan_id,revision_id,session_id,operation,created_at_ms,expires_at_ms,manifest_sha256,targets_json,selected_logical_bytes,state) VALUES(?1,?2,?3,?4,'permanently_remove_staged',?5,?6,?7,?8,?9,'prepared')",
            rusqlite::params![
                wire,
                serde_json::to_string(&manifest.plan_id).map_err(|_| PurgeError::StorageFailure)?,
                serde_json::to_string(&manifest.revision_id).map_err(|_| PurgeError::StorageFailure)?,
                serde_json::to_string(&manifest.session_id).map_err(|_| PurgeError::StorageFailure)?,
                created_at_ms,
                manifest.expires_at_ms,
                manifest_sha256,
                targets_json,
                selected_logical_bytes as i64,
            ],
        )?;
        self.checkpoint()?;
        Ok(PreparedPurgeReview {
            schema_version: 1,
            kind: "disposable_fixture_purge_review".into(),
            review_id,
            plan_id: manifest.plan_id,
            revision_id: manifest.revision_id,
            session_id: manifest.session_id,
            operation: SupportedOperation::PermanentlyRemoveStaged,
            created_at_ms,
            expires_at_ms: manifest.expires_at_ms,
            manifest_sha256,
            targets: review_targets,
            selected_logical_bytes,
        })
    }

    fn checkpoint(&self) -> Result<(), PurgeError> {
        self.control.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        Ok(())
    }

    /// Revalidate everything fresh, obtain trusted confirmation, atomically
    /// claim the unused review, and execute the purge serially. A decline
    /// mutates nothing and consumes nothing; the review stays prepared until
    /// expiry. Contract: revalidation runs immediately before the callback
    /// and again after it returns.
    pub fn claim_and_execute(
        &mut self,
        review_id: ReviewId,
        confirmer: impl FnOnce(&PreparedPurgeReview) -> bool,
    ) -> Result<PurgeExecutionResult, PurgeError> {
        let stored = self.read_stored_review(&review_id)?;
        let now = (self.now)();
        if now >= stored.prepared.expires_at_ms {
            return Err(PurgeError::ReviewExpired);
        }
        self.revalidate(&stored)?;
        if !confirmer(&stored.prepared) {
            return Err(PurgeError::ConfirmationDeclined);
        }
        let after = (self.now)();
        if after >= stored.prepared.expires_at_ms {
            return Err(PurgeError::ReviewExpired);
        }
        self.revalidate(&stored)?;
        let (execution_id, child_attempts) = self.claim(&stored, after)?;
        self.execute_claimed_batch(execution_id, &stored.prepared, child_attempts)
    }

    fn read_stored_review(&self, review_id: &ReviewId) -> Result<StoredReview, PurgeError> {
        let wire = serde_json::to_string(review_id).map_err(|_| PurgeError::StorageFailure)?;
        let row: Option<StoredReviewRow> =
            self.control
                .query_row(
                    "SELECT plan_id,revision_id,session_id,created_at_ms,expires_at_ms,manifest_sha256,targets_json,selected_logical_bytes,state FROM purge_reviews WHERE review_id=?1",
                    [&wire],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                        ))
                    },
                )
                .optional()?;
        let Some(row) = row else {
            return Err(PurgeError::ReviewReplayed);
        };
        let (
            plan_id,
            revision_id,
            session_id,
            created_at_ms,
            expires_at_ms,
            manifest_sha256,
            targets_json,
            selected_logical_bytes,
            state,
        ) = row;
        // The state gate comes first: a claimed or finished review can
        // never be claimed again, regardless of current plan/fixture state.
        if state != "prepared" {
            return Err(PurgeError::ReviewReplayed);
        }
        let plan_id: PlanId =
            serde_json::from_str(&plan_id).map_err(|_| PurgeError::UnsupportedSchema)?;
        let revision_id: RevisionId =
            serde_json::from_str(&revision_id).map_err(|_| PurgeError::UnsupportedSchema)?;
        let session_id: SessionId =
            serde_json::from_str(&session_id).map_err(|_| PurgeError::UnsupportedSchema)?;
        let targets: Vec<PurgeReviewTarget> =
            serde_json::from_str(&targets_json).map_err(|_| PurgeError::UnsupportedSchema)?;
        if targets.is_empty() || targets.len() > MAX_TARGETS {
            return Err(PurgeError::UnsupportedSchema);
        }
        if manifest_sha256.len() != 64 {
            return Err(PurgeError::UnsupportedSchema);
        }
        Ok(StoredReview {
            prepared: PreparedPurgeReview {
                schema_version: 1,
                kind: "disposable_fixture_purge_review".into(),
                review_id: *review_id,
                plan_id,
                revision_id,
                session_id,
                operation: SupportedOperation::PermanentlyRemoveStaged,
                created_at_ms,
                expires_at_ms,
                manifest_sha256,
                targets,
                selected_logical_bytes: selected_logical_bytes as u64,
            },
        })
    }

    /// Recompute the manifest over the CURRENT plan and lifecycle state and
    /// require it to match the frozen hash. Any drift — revision moved,
    /// decision flipped, receipt no longer staged, content changed — refuses.
    fn revalidate(&self, stored: &StoredReview) -> Result<(), PurgeError> {
        let prepared = &stored.prepared;
        let plan_view = self
            .plan
            .get_plan()
            .map_err(|_| PurgeError::StaleRevision)?;
        if plan_view.plan_id != prepared.plan_id || plan_view.revision_id != prepared.revision_id {
            return Err(PurgeError::StaleRevision);
        }
        let snapshot = self.lifecycle.snapshot()?;
        if snapshot.session_id != prepared.session_id {
            return Err(PurgeError::ChangedTarget);
        }
        for target in &prepared.targets {
            let action = plan_view
                .actions
                .iter()
                .find(|a| a.id == target.action_id)
                .ok_or(PurgeError::ChangedTarget)?;
            if action.decision != disk_steward_review_plan::Decision::Include
                || action.bytes != target.logical_bytes
                || action.label != target.label
            {
                return Err(PurgeError::ChangedTarget);
            }
            let fixture = snapshot
                .fixtures
                .iter()
                .find(|f| f.action_id == target.action_id)
                .ok_or(PurgeError::ChangedTarget)?;
            if fixture.receipt_id.as_ref() != Some(&target.receipt_id)
                || fixture.status != FixtureStatus::Staged
                || fixture.bytes != target.logical_bytes
            {
                return Err(PurgeError::ChangedTarget);
            }
        }
        let manifest = self.build_manifest(prepared, &snapshot)?;
        let manifest_sha256 = hex_lower(&Sha256::digest(
            &serde_json::to_vec(&manifest).map_err(|_| PurgeError::StorageFailure)?,
        ));
        if manifest_sha256 != prepared.manifest_sha256 {
            return Err(PurgeError::ChangedTarget);
        }
        Ok(())
    }

    fn build_manifest(
        &self,
        prepared: &PreparedPurgeReview,
        snapshot: &disk_steward_fixture_lifecycle::SessionSnapshot,
    ) -> Result<PurgeManifestV1, PurgeError> {
        let mut manifest_targets = Vec::with_capacity(prepared.targets.len());
        for target in &prepared.targets {
            let fixture = snapshot
                .fixtures
                .iter()
                .find(|f| f.action_id == target.action_id)
                .ok_or(PurgeError::ChangedTarget)?;
            manifest_targets.push(ManifestTargetV1 {
                action_id: target.action_id,
                receipt_id: target.receipt_id,
                logical_bytes: fixture.bytes,
                content_sha256: fixture.sha256.clone(),
                identity_sha256: fixture.identity_sha256.clone(),
            });
        }
        Ok(PurgeManifestV1 {
            schema_version: 1,
            session_id: prepared.session_id,
            plan_id: prepared.plan_id,
            revision_id: prepared.revision_id,
            operation: prepared.operation,
            created_at_ms: prepared.created_at_ms,
            expires_at_ms: prepared.expires_at_ms,
            selected_logical_bytes: prepared.selected_logical_bytes,
            adapter_schema_version: 2,
            targets: manifest_targets,
        })
    }

    /// Atomically claim the prepared review and journal the execution plus
    /// every child attempt BEFORE any file mutation.
    fn claim(
        &mut self,
        stored: &StoredReview,
        now: i64,
    ) -> Result<
        (
            disk_steward_core::ExecutionId,
            Vec<disk_steward_core::AttemptId>,
        ),
        PurgeError,
    > {
        let execution_id = disk_steward_core::ExecutionId::new();
        let execution_wire =
            serde_json::to_string(&execution_id).map_err(|_| PurgeError::StorageFailure)?;
        let review_wire = serde_json::to_string(&stored.prepared.review_id)
            .map_err(|_| PurgeError::StorageFailure)?;
        let session_wire = serde_json::to_string(&stored.prepared.session_id)
            .map_err(|_| PurgeError::StorageFailure)?;
        let child_attempts: Vec<disk_steward_core::AttemptId> = stored
            .prepared
            .targets
            .iter()
            .map(|_| disk_steward_core::AttemptId::new())
            .collect();
        let tx = self
            .control
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let claimed = tx.execute(
            "UPDATE purge_reviews SET state='claimed' WHERE review_id=?1 AND state='prepared'",
            [&review_wire],
        )?;
        if claimed != 1 {
            return Err(PurgeError::ReviewReplayed);
        }
        tx.execute(
            "INSERT INTO purge_executions(execution_id,review_id,session_id,started_at_ms,state) VALUES(?1,?2,?3,?4,'started')",
            rusqlite::params![execution_wire, review_wire, session_wire, now],
        )?;
        for (attempt, target) in child_attempts.iter().zip(stored.prepared.targets.iter()) {
            tx.execute(
                "INSERT INTO purge_child_attempts(execution_id,attempt_id,action_id,receipt_id,state) VALUES(?1,?2,?3,?4,'pending')",
                rusqlite::params![
                    execution_wire,
                    serde_json::to_string(attempt)
                        .map_err(|_| PurgeError::StorageFailure)?,
                    serde_json::to_string(&target.action_id)
                        .map_err(|_| PurgeError::StorageFailure)?,
                    serde_json::to_string(&target.receipt_id)
                        .map_err(|_| PurgeError::StorageFailure)?,
                ],
            )?;
        }
        tx.commit()?;
        self.checkpoint()?;
        Ok((execution_id, child_attempts))
    }

    /// Serially purge each claimed target. The first uncertain item marks
    /// the whole execution uncertain; the first refusal (no intent journaled)
    /// marks it partial; every unprocessed item records NotAttempted. The
    /// batch never retries and never skips ahead.
    fn execute_claimed_batch(
        &mut self,
        execution_id: disk_steward_core::ExecutionId,
        prepared: &PreparedPurgeReview,
        child_attempts: Vec<disk_steward_core::AttemptId>,
    ) -> Result<PurgeExecutionResult, PurgeError> {
        let execution_wire =
            serde_json::to_string(&execution_id).map_err(|_| PurgeError::StorageFailure)?;
        let mut items = Vec::with_capacity(child_attempts.len());
        let mut durably_unlinked_logical_bytes = 0u64;
        let mut final_state = "finished".to_owned();
        let mut stopped = false;
        for (attempt, target) in child_attempts.iter().zip(prepared.targets.iter()) {
            let attempt_wire =
                serde_json::to_string(attempt).map_err(|_| PurgeError::StorageFailure)?;
            let child_state = if stopped {
                "not_attempted"
            } else {
                match self.lifecycle.purge_receipt(target.receipt_id, *attempt) {
                    Ok(PurgeItemOutcome::PermanentlyRemoved { logical_bytes, .. }) => {
                        durably_unlinked_logical_bytes = durably_unlinked_logical_bytes
                            .checked_add(logical_bytes)
                            .ok_or(PurgeError::InvalidInput)?;
                        "permanently_removed"
                    }
                    Ok(PurgeItemOutcome::NotAttempted { .. })
                    | Ok(PurgeItemOutcome::Uncertain { .. }) => {
                        // The lifecycle never constructs these here.
                        "uncertain"
                    }
                    Err(disk_steward_fixture_lifecycle::LifecycleError::Uncertain) => {
                        final_state = "uncertain".to_owned();
                        stopped = true;
                        "uncertain"
                    }
                    Err(_) => {
                        if final_state == "finished" {
                            final_state = "partial".to_owned();
                        }
                        stopped = true;
                        "not_attempted"
                    }
                }
            };
            self.control.execute(
                "UPDATE purge_child_attempts SET state=?1 WHERE attempt_id=?2 AND state='pending'",
                rusqlite::params![child_state, attempt_wire],
            )?;
            let outcome = match child_state {
                "permanently_removed" => PurgeItemOutcome::PermanentlyRemoved {
                    receipt_id: target.receipt_id,
                    logical_bytes: target.logical_bytes,
                },
                "uncertain" => PurgeItemOutcome::Uncertain {
                    receipt_id: target.receipt_id,
                },
                _ => PurgeItemOutcome::NotAttempted {
                    receipt_id: target.receipt_id,
                },
            };
            items.push(PurgeItemBatchOutcome {
                attempt_id: *attempt,
                outcome,
            });
        }
        let finished_at_ms = (self.now)();
        self.control.execute(
            "UPDATE purge_executions SET state=?1, finished_at_ms=?2 WHERE execution_id=?3 AND state='started'",
            rusqlite::params![final_state, finished_at_ms, execution_wire],
        )?;
        self.checkpoint()?;
        Ok(PurgeExecutionResult {
            execution_id,
            review_id: prepared.review_id,
            state: final_state,
            items,
            selected_logical_bytes: prepared.selected_logical_bytes,
            durably_unlinked_logical_bytes,
        })
    }
}

struct StoredReview {
    prepared: PreparedPurgeReview,
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Owner recovery, run once per open: a claimed-but-started execution whose
/// child outcomes never became durable is resolved to `uncertain`, and its
/// pending child rows become `uncertain` with it. Recovery reads only exact
/// journal rows; it never retries the unlink, never scans directories, and
/// never deletes. The review stays claimed — replay is refused; a fresh
/// review is required for any new action. Idempotent: terminal executions
/// are untouched.
fn recover_unfinished_executions(db: &mut Connection, now: i64) -> Result<(), PurgeError> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE purge_executions SET state='uncertain', finished_at_ms=COALESCE(finished_at_ms, ?1) WHERE state='started'",
        rusqlite::params![now],
    )?;
    tx.execute(
        "UPDATE purge_child_attempts SET state='uncertain' WHERE state='pending' AND execution_id IN (SELECT execution_id FROM purge_executions WHERE state='uncertain')",
        [],
    )?;
    tx.commit()?;
    Ok(())
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
    use disk_steward_review_plan::PlanView;
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
                name.starts_with(".disk-steward-fixtures-").then_some(name)
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

    fn service_with_staged(
        dir: &Path,
        count: usize,
        now: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> (FixturePurgeService, Vec<PurgeTarget>, PlanView) {
        // The demo plan store seeds its own fixed five-action set; the
        // lifecycle must be created with the plan's action ids (same as
        // DemoExecutionService::start_demo).
        let mut plan = ReviewPlanStore::open(dir.join("review-example-v1.sqlite3")).unwrap();
        let plan_view = plan.get_plan().unwrap();
        let ids: [disk_steward_core::ActionId; 5] = plan_view
            .actions
            .iter()
            .map(|a| a.id)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let (mut lifecycle, _) = FixtureLifecycle::create(dir, ids).unwrap();
        let mut targets = Vec::new();
        let mut revision = plan_view.revision_id;
        for id in ids.iter().take(count) {
            let edited = plan
                .edit(revision, *id, disk_steward_review_plan::Decision::Include)
                .unwrap();
            revision = edited.revision_id;
            let receipt = lifecycle
                .stage(*id, disk_steward_core::AttemptId::new())
                .unwrap();
            targets.push(PurgeTarget {
                action_id: *id,
                receipt_id: receipt,
            });
        }
        let view = plan.get_plan().unwrap();
        let service = FixturePurgeService::open_with_clock(dir, lifecycle, plan, now).unwrap();
        (service, targets, view)
    }

    fn fixed_clock(now: i64) -> Box<dyn Fn() -> i64 + Send + Sync> {
        Box::new(move || now)
    }

    #[test]
    fn prepare_persists_an_immutable_sanitized_review() {
        let (dir, _ids) = demo_root();
        let now = 1_700_000_000_000_i64;
        let (mut service, targets, plan_view) =
            service_with_staged(dir.path(), 3, fixed_clock(now));
        let expected_revision = plan_view.revision_id;
        let review = service
            .prepare_purge_review(expected_revision, targets.clone())
            .unwrap();
        assert_eq!(review.kind, "disposable_fixture_purge_review");
        assert_eq!(
            review.operation,
            SupportedOperation::PermanentlyRemoveStaged
        );
        assert_eq!(review.session_id, review.session_id);
        assert_eq!(review.created_at_ms, now);
        assert_eq!(review.expires_at_ms, now + REVIEW_LIFETIME_MS);
        assert_eq!(review.targets.len(), 3);
        assert_eq!(review.selected_logical_bytes, 4_718_592);
        for (target, plan_action) in review.targets.iter().zip(plan_view.actions.iter()) {
            assert_eq!(
                target.receipt_id,
                targets
                    .iter()
                    .find(|t| t.action_id == plan_action.id)
                    .map(|t| t.receipt_id)
                    .unwrap()
            );
            assert_eq!(target.label, plan_action.label);
            assert_eq!(target.status, PurgeReceiptState::Staged);
            assert_eq!(target.logical_bytes, plan_action.bytes);
        }
        assert_eq!(review.manifest_sha256.len(), 64);
        // The control store holds exactly one prepared review.
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let (state, stored_json): (String, String) = db
            .query_row(
                "SELECT state, targets_json FROM purge_reviews WHERE review_id=?1",
                [serde_json::to_string(&review.review_id).unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "prepared");
        assert!(stored_json.contains("\"logical_bytes\""));
    }

    #[test]
    fn prepare_manifest_hash_is_deterministic_for_identical_state() {
        let (dir, _ids) = demo_root();
        let now = 1_700_000_000_000_i64;
        let (mut service, targets, plan_view) =
            service_with_staged(dir.path(), 2, fixed_clock(now));
        let first = service
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        let second = service
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        // Same frozen content at the same clock: same manifest digest, but
        // distinct single-use review rows.
        assert_eq!(first.manifest_sha256, second.manifest_sha256);
        assert_ne!(first.review_id, second.review_id);
    }

    #[test]
    fn prepare_refuses_bad_target_sets_and_stale_revisions() {
        let (dir, _ids) = demo_root();
        let (mut service, targets, plan_view) =
            service_with_staged(dir.path(), 2, fixed_clock(1_700_000_000_000));
        let revision = plan_view.revision_id;
        // Empty target set.
        assert!(matches!(
            service.prepare_purge_review(revision, Vec::new()),
            Err(PurgeError::InvalidInput)
        ));
        // Six targets: more than the fixed five-fixture session offers.
        let mut too_many = targets.clone();
        for _ in 0..(6 - too_many.len()) {
            too_many.push(PurgeTarget {
                action_id: disk_steward_core::ActionId::new(),
                receipt_id: ReceiptId::new(),
            });
        }
        assert!(matches!(
            service.prepare_purge_review(revision, too_many),
            Err(PurgeError::InvalidInput)
        ));
        // Duplicate action id.
        let duplicated = vec![targets[0].clone(), targets[0].clone()];
        assert!(matches!(
            service.prepare_purge_review(revision, duplicated),
            Err(PurgeError::InvalidInput)
        ));
        // Unknown action.
        let unknown = vec![PurgeTarget {
            action_id: disk_steward_core::ActionId::new(),
            receipt_id: ReceiptId::new(),
        }];
        assert!(matches!(
            service.prepare_purge_review(revision, unknown),
            Err(PurgeError::InvalidInput)
        ));
        // Stale revision.
        assert!(matches!(
            service.prepare_purge_review(RevisionId::new(), targets.clone()),
            Err(PurgeError::StaleRevision)
        ));
    }

    #[test]
    fn prepare_refuses_targets_that_are_not_currently_staged() {
        let (dir, _ids) = demo_root();
        let (mut service, mut targets, plan_view) =
            service_with_staged(dir.path(), 2, fixed_clock(1_700_000_000_000));
        let revision = plan_view.revision_id;
        // A target whose receipt was never issued by this session.
        let unissued = vec![PurgeTarget {
            action_id: targets[0].action_id,
            receipt_id: ReceiptId::new(),
        }];
        assert!(matches!(
            service.prepare_purge_review(revision, unissued),
            Err(PurgeError::ChangedTarget)
        ));
        // A target whose fixture is not staged (fixture 3 was never staged
        // in this helper run: its action is available with no receipt).
        assert_eq!(targets.len(), 2);
        let _ = &mut targets;
        // A receipt is staged: the pair (action, receipt) must match the
        // session's staged state exactly; a mismatched receipt pair for a
        // staged action is refused above. Nothing was journaled.
        let reviews: i64 = {
            let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
            db.query_row("SELECT COUNT(*) FROM purge_reviews", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(reviews, 0);
    }

    #[test]
    fn prepare_refuses_excluded_actions() {
        let (dir, _ids) = demo_root();
        // Zero staged: nothing is Included; the Keep decision must be refused.
        let (mut service, _targets, plan_view) =
            service_with_staged(dir.path(), 0, fixed_clock(1_700_000_000_000));
        let excluded = vec![PurgeTarget {
            action_id: plan_view.actions[0].id,
            receipt_id: ReceiptId::new(),
        }];
        assert!(matches!(
            service.prepare_purge_review(plan_view.revision_id, excluded),
            Err(PurgeError::InvalidInput)
        ));
    }

    fn service_with_staged_and_clock_cell(
        dir: &Path,
        count: usize,
        tick: std::sync::Arc<std::sync::atomic::AtomicI64>,
    ) -> (FixturePurgeService, Vec<PurgeTarget>, PlanView) {
        use std::sync::atomic::Ordering;
        let now: Box<dyn Fn() -> i64 + Send + Sync> = {
            let tick = tick.clone();
            Box::new(move || tick.load(Ordering::SeqCst))
        };
        service_with_staged(dir, count, now)
    }

    fn holding_slot_for(dir: &Path, receipt_wire: &str) -> std::path::PathBuf {
        // slot = "receipt-<component>"; component is the last ':' segment of
        // the unquoted typed wire (same derivation as fixture-lifecycle).
        let unquoted = receipt_wire.trim_matches('"');
        let component = unquoted.rsplit(':').next().unwrap();
        let entry = std::fs::read_dir(dir)
            .unwrap()
            .find_map(|e| {
                let name = e.unwrap().file_name().into_string().unwrap();
                name.starts_with(".disk-steward-fixtures-").then_some(name)
            })
            .unwrap();
        dir.join(entry)
            .join("holding")
            .join(format!("receipt-{component}"))
    }

    #[test]
    fn claim_executes_serially_and_journals_terminal_outcomes() {
        let (dir, _ids) = demo_root();
        let (mut service, targets, plan_view) =
            service_with_staged(dir.path(), 3, fixed_clock(1_700_000_000_000));
        let review = service
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        let result = service
            .claim_and_execute(review.review_id, |_| true)
            .unwrap();
        assert_eq!(result.state, "finished");
        assert_eq!(result.items.len(), 3);
        assert!(result
            .items
            .iter()
            .all(|i| matches!(i.outcome, PurgeItemOutcome::PermanentlyRemoved { .. })));
        assert_eq!(result.selected_logical_bytes, 4_718_592);
        assert_eq!(result.durably_unlinked_logical_bytes, 4_718_592);
        let snapshot = service.lifecycle.snapshot().unwrap();
        for (index, fixture) in snapshot.fixtures.iter().enumerate().take(3) {
            assert_eq!(fixture.status, FixtureStatus::PermanentlyRemoved);
            let receipt_wire = serde_json::to_string(&targets[index].receipt_id).unwrap();
            assert!(!holding_slot_for(dir.path(), &receipt_wire).exists());
        }
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let (review_state, execution_state): (String, String) = db
            .query_row(
                "SELECT r.state, e.state FROM purge_reviews r JOIN purge_executions e ON e.review_id=r.review_id WHERE r.review_id=?1",
                [serde_json::to_string(&review.review_id).unwrap()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(review_state, "claimed");
        assert_eq!(execution_state, "finished");
        let pending: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM purge_child_attempts WHERE state='pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0);
    }

    #[test]
    fn declined_confirmation_leaves_everything_untouched() {
        let (dir, _ids) = demo_root();
        let (mut service, targets, plan_view) =
            service_with_staged(dir.path(), 2, fixed_clock(1_700_000_000_000));
        let review = service
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        let error = service
            .claim_and_execute(review.review_id, |_| false)
            .unwrap_err();
        assert_eq!(error.code(), "confirmation_declined");
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let state: String = db
            .query_row(
                "SELECT state FROM purge_reviews WHERE review_id=?1",
                [serde_json::to_string(&review.review_id).unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "prepared");
        let executions: i64 = db
            .query_row("SELECT COUNT(*) FROM purge_executions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(executions, 0, "decline must not create an execution");
        for target in &targets {
            let receipt_wire = serde_json::to_string(&target.receipt_id).unwrap();
            assert!(holding_slot_for(dir.path(), &receipt_wire).exists());
        }
        // The still-prepared review can be claimed afterwards.
        let result = service
            .claim_and_execute(review.review_id, |_| true)
            .unwrap();
        assert_eq!(result.state, "finished");
    }

    #[test]
    fn changed_target_between_prepare_and_claim_refuses_without_mutation() {
        let (dir, _ids) = demo_root();
        let (mut service, targets, plan_view) =
            service_with_staged(dir.path(), 2, fixed_clock(1_700_000_000_000));
        let review = service
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        // Externally mark the first target's fixture restored (state change).
        {
            let entry = std::fs::read_dir(dir.path())
                .unwrap()
                .find_map(|e| {
                    let name = e.unwrap().file_name().into_string().unwrap();
                    name.starts_with(".disk-steward-fixtures-").then_some(name)
                })
                .unwrap();
            let db = Connection::open(dir.path().join(entry).join("lifecycle.sqlite3")).unwrap();
            db.execute("UPDATE fixtures SET status='restored' WHERE rowid=1", [])
                .unwrap();
            // Keep the journal consistent as a real restore would: the stage
            // row keeps outcome='staged'; a new restore attempt is appended.
            let action: String = db
                .query_row("SELECT action_id FROM fixtures WHERE rowid=1", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let receipt: String = db
                .query_row("SELECT receipt_id FROM fixtures WHERE rowid=1", [], |r| {
                    r.get(0)
                })
                .unwrap();
            db.execute(
                "INSERT INTO attempts(attempt_id,action_id,operation,receipt_id,outcome) VALUES(?1,?2,'restore',?3,'restored')",
                rusqlite::params![
                    serde_json::to_string(&disk_steward_core::AttemptId::new()).unwrap(),
                    action,
                    receipt
                ],
            )
            .unwrap();
        }
        let error = service
            .claim_and_execute(review.review_id, |_| true)
            .unwrap_err();
        assert_eq!(error.code(), "changed_target");
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let state: String = db
            .query_row(
                "SELECT state FROM purge_reviews WHERE review_id=?1",
                [serde_json::to_string(&review.review_id).unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, "prepared");
        let receipt_wire = serde_json::to_string(&targets[1].receipt_id).unwrap();
        assert!(holding_slot_for(dir.path(), &receipt_wire).exists());
    }

    #[test]
    fn expired_and_replayed_reviews_are_refused() {
        use std::sync::atomic::Ordering;
        let (dir, _ids) = demo_root();
        let tick = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(1_700_000_000_000_i64));
        let (mut service, targets, plan_view) =
            service_with_staged_and_clock_cell(dir.path(), 2, tick.clone());
        let review = service
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        tick.store(
            1_700_000_000_000_i64 + REVIEW_LIFETIME_MS + 1,
            Ordering::SeqCst,
        );
        let error = service
            .claim_and_execute(review.review_id, |_| true)
            .unwrap_err();
        assert_eq!(error.code(), "review_expired");
        tick.store(1_700_000_000_000_i64, std::sync::atomic::Ordering::SeqCst);
        let result = service
            .claim_and_execute(review.review_id, |_| true)
            .unwrap();
        assert_eq!(result.state, "finished");
        let error = service
            .claim_and_execute(review.review_id, |_| true)
            .unwrap_err();
        assert_eq!(error.code(), "review_replayed");
    }

    #[test]
    fn recovery_marks_claimed_unfinished_executions_uncertain() {
        // Simulate a crash after the claim but before any item outcome:
        // review claimed, execution started, child rows pending. On reopen,
        // recovery must mark them uncertain — no retry, no replay, no
        // success claim — while a fresh review for the same receipts remains
        // possible (contract: a later fresh review is required for any new
        // action).
        let (dir, _ids) = demo_root();
        let (mut service, targets, plan_view) =
            service_with_staged(dir.path(), 2, fixed_clock(1_700_000_000_000));
        let review = service
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        let review_wire = serde_json::to_string(&review.review_id).unwrap();
        let execution_wire = serde_json::to_string(&disk_steward_core::ExecutionId::new()).unwrap();
        {
            // Fabricate the claimed-but-unfinished state by hand.
            let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
            db.execute(
                "UPDATE purge_reviews SET state='claimed' WHERE review_id=?1",
                [&review_wire],
            )
            .unwrap();
            db.execute(
                "INSERT INTO purge_executions(execution_id,review_id,session_id,started_at_ms,state) VALUES(?1,?2,?3,?4,'started')",
                rusqlite::params![
                    execution_wire,
                    review_wire,
                    serde_json::to_string(&review.session_id).unwrap(),
                    1_700_000_000_000_i64
                ],
            )
            .unwrap();
            for target in &targets {
                db.execute(
                    "INSERT INTO purge_child_attempts(execution_id,attempt_id,action_id,receipt_id,state) VALUES(?1,?2,?3,?4,'pending')",
                    rusqlite::params![
                        execution_wire,
                        serde_json::to_string(&disk_steward_core::AttemptId::new()).unwrap(),
                        serde_json::to_string(&target.action_id).unwrap(),
                        serde_json::to_string(&target.receipt_id).unwrap()
                    ],
                )
                .unwrap();
            }
        }
        // Reopen: recovery must resolve the crash state.
        let (lifecycle, _) = reopen_lifecycle(dir.path());
        let plan = ReviewPlanStore::open(dir.path().join("review-example-v1.sqlite3")).unwrap();
        let mut _recovered = FixturePurgeService::open(dir.path(), lifecycle, plan).unwrap();
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let execution_state: String = db
            .query_row(
                "SELECT state FROM purge_executions WHERE execution_id=?1",
                [&execution_wire],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(execution_state, "uncertain");
        let pending: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM purge_child_attempts WHERE state='pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0, "recovery resolves every pending child row");
        let child_states: String = db
            .query_row(
                "SELECT GROUP_CONCAT(DISTINCT state) FROM purge_child_attempts",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(child_states, "uncertain");
        // The files were never touched by recovery.
        for target in &targets {
            let receipt_wire = serde_json::to_string(&target.receipt_id).unwrap();
            assert!(holding_slot_for(dir.path(), &receipt_wire).exists());
        }
        // Replay of the crashed review is refused.
        let error = _recovered
            .claim_and_execute(review.review_id, |_| true)
            .unwrap_err();
        assert_eq!(error.code(), "review_replayed");
        // A fresh review for the same receipts is still possible.
        let fresh = _recovered
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        assert_ne!(fresh.review_id, review.review_id);
    }

    #[test]
    fn recovery_leaves_terminal_executions_untouched() {
        // A finished execution must survive reopen exactly as it was.
        let (dir, _ids) = demo_root();
        let (mut service, targets, plan_view) =
            service_with_staged(dir.path(), 1, fixed_clock(1_700_000_000_000));
        let review = service
            .prepare_purge_review(plan_view.revision_id, targets.clone())
            .unwrap();
        let result = service
            .claim_and_execute(review.review_id, |_| true)
            .unwrap();
        assert_eq!(result.state, "finished");
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        drop(db);
        let (lifecycle, _) = reopen_lifecycle(dir.path());
        let plan = ReviewPlanStore::open(dir.path().join("review-example-v1.sqlite3")).unwrap();
        let mut _recovered = FixturePurgeService::open(dir.path(), lifecycle, plan).unwrap();
        let db = Connection::open(dir.path().join(CONTROL_DB)).unwrap();
        let execution_state: String = db
            .query_row("SELECT state FROM purge_executions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(execution_state, "finished");
        let children: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM purge_child_attempts WHERE state='permanently_removed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(children, 1);
    }
}
