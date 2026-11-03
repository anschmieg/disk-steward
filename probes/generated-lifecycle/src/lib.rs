//! Fixture-only stage/restore/purge validation for generated pytest records.
//!
//! This crate owns its private temporary fixture directory and accepts no
//! paths, filenames, commands, or renderer input. It is a probe, not a
//! production cleanup adapter.

use disk_steward_core::{ActionId, ApprovalId, ExecutionId, ReceiptId, ReviewId};
use disk_steward_generated_evidence::{classify_pytest_record, RecordKind as FormatKind};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use tempfile::TempDir;

const REVIEW_TTL_MS: i64 = 5 * 60 * 1000;
const MAX_REVIEWS: usize = 32;
const MAX_ATTEMPTS: usize = 32;
const MAX_RECORD_BYTES: u64 = 64 * 1024;
const RESERVED_BYTES: u64 = 2 * 1024 * 1024 * 1024 + 64 * 1024 * 1024;
const SENTINEL_NAME: &str = "local-notes.txt";
const SENTINEL_BYTES: &[u8] = b"valuable local note: preserve this unselected file\n";

const NODEIDS_BYTES: &[u8] =
    include_bytes!("../../generated_cache/fixtures/pytest-9.2.0/nodeids.json");
const LASTFAILED_BYTES: &[u8] =
    include_bytes!("../../generated_cache/fixtures/pytest-9.2.0/lastfailed.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecordKind {
    NodeIds,
    LastFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Stage,
    Restore,
    Purge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSummary {
    review_id: ReviewId,
    operation: Operation,
    action_id: ActionId,
    receipt_id: ReceiptId,
    kind: RecordKind,
    logical_bytes: u64,
    expires_at_unix_ms: i64,
}

impl ReviewSummary {
    pub fn review_id(&self) -> ReviewId {
        self.review_id
    }
    pub fn operation(&self) -> Operation {
        self.operation
    }
    pub fn action_id(&self) -> ActionId {
        self.action_id
    }
    pub fn receipt_id(&self) -> ReceiptId {
        self.receipt_id
    }
    pub fn kind(&self) -> RecordKind {
        self.kind
    }
    pub fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }
    pub fn expires_at_unix_ms(&self) -> i64 {
        self.expires_at_unix_ms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Staged {
        action_id: ActionId,
        receipt_id: ReceiptId,
    },
    Restored {
        action_id: ActionId,
        receipt_id: ReceiptId,
    },
    PermanentlyRemoved {
        action_id: ActionId,
        receipt_id: ReceiptId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeError {
    Unsupported,
    InsufficientSpace,
    Io,
    ApprovalLedger,
    InvalidTime,
    Expired,
    ReviewLimit,
    AttemptLimit,
    WrongOperation,
    ReviewUnavailable,
    ReviewReused,
    NotApproved,
    WrongState,
    WrongReceipt,
    Changed,
    DestinationOccupied,
    Uncertain,
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "the fixture lifecycle probe is unsupported on this platform",
            Self::InsufficientSpace => "the temporary fixture volume is below its reserve",
            Self::Io => "the private generated-fixture operation failed",
            Self::ApprovalLedger => "the fixture approval ledger refused the operation",
            Self::InvalidTime => "the supplied fixture clock value is invalid",
            Self::Expired => "the fixture review expired",
            Self::ReviewLimit => "the fixture review limit was reached",
            Self::AttemptLimit => "the fixture attempt limit was reached",
            Self::WrongOperation => "the fixture review belongs to another operation",
            Self::ReviewUnavailable => "the fixture review is unavailable",
            Self::ReviewReused => "the fixture review or approval was already used",
            Self::NotApproved => "the fixture review has not been approved",
            Self::WrongState => "the fixture record is not in the required state",
            Self::WrongReceipt => "the fixture receipt is not current",
            Self::Changed => "the generated fixture changed and requires inspection",
            Self::DestinationOccupied => "the original fixture name is occupied",
            Self::Uncertain => "the fixture outcome is uncertain and will not be retried",
        })
    }
}
impl std::error::Error for ProbeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Available,
    Staged,
    Restored,
    PermanentlyRemoved,
    Uncertain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    links: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Proof {
    stamp: Stamp,
    sha256: [u8; 32],
}

#[derive(Clone)]
struct ActionState {
    action_id: ActionId,
    phase: Phase,
    receipt: Option<ReceiptId>,
    proof: Proof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReviewPhase {
    Prepared,
    Approved,
    Executed,
    Rejected,
}

#[derive(Clone, Debug, Serialize)]
struct Manifest {
    schema: u32,
    adapter_version: &'static str,
    operation: OperationWire,
    action_id: String,
    receipt_id: String,
    kind: String,
    bytes: u64,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    sha256: String,
    created_at_unix_ms: i64,
    expires_at_unix_ms: i64,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum OperationWire {
    Stage,
    Restore,
    Purge,
}

#[derive(Clone)]
struct Review {
    summary: ReviewSummary,
    approval_id: ApprovalId,
    manifest_digest: String,
    phase: ReviewPhase,
    proof: Proof,
    created_at_unix_ms: i64,
}

pub struct GeneratedLifecycleProbe {
    _temp: TempDir,
    temp_parent: File,
    temp_name: String,
    root: File,
    source: File,
    holding: File,
    ledger: approval_ledger::ApprovalLedger,
    actions: BTreeMap<RecordKind, ActionState>,
    reviews: BTreeMap<ReviewId, Review>,
    attempts: usize,
    #[cfg(test)]
    fail_finish: bool,
    #[cfg(test)]
    last_intent: Option<ExecutionId>,
}

impl GeneratedLifecycleProbe {
    /// Creates a private probe fixture from the committed pytest 9.2.0 corpus.
    /// The temporary volume must have at least 2 GiB + 64 MiB available.
    pub fn create() -> Result<Self, ProbeError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use rustix::fs::{fstat, fstatvfs, FileType};
            use std::os::unix::fs::PermissionsExt;

            let temp = tempfile::tempdir().map_err(|_| ProbeError::Io)?;
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))
                .map_err(|_| ProbeError::Io)?;
            let temp_parent = File::open(temp.path().parent().ok_or(ProbeError::Io)?)
                .map_err(|_| ProbeError::Io)?;
            let temp_name = temp
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(ProbeError::Io)?
                .to_owned();
            let root = File::open(temp.path()).map_err(|_| ProbeError::Io)?;
            let stats = fstatvfs(&root).map_err(|_| ProbeError::Io)?;
            let available = stats.f_bavail.checked_mul(stats.f_frsize).unwrap_or(0);
            if available < RESERVED_BYTES {
                return Err(ProbeError::InsufficientSpace);
            }
            if FileType::from_raw_mode(fstat(&root).map_err(|_| ProbeError::Io)?.st_mode)
                != FileType::Directory
            {
                return Err(ProbeError::Io);
            }
            fs::create_dir(temp.path().join("source")).map_err(|_| ProbeError::Io)?;
            fs::create_dir(temp.path().join("holding")).map_err(|_| ProbeError::Io)?;
            for name in ["source", "holding"] {
                fs::set_permissions(temp.path().join(name), fs::Permissions::from_mode(0o700))
                    .map_err(|_| ProbeError::Io)?;
            }
            let source = File::open(temp.path().join("source")).map_err(|_| ProbeError::Io)?;
            let holding = File::open(temp.path().join("holding")).map_err(|_| ProbeError::Io)?;
            let source_stat = fstat(&source).map_err(|_| ProbeError::Io)?;
            let holding_stat = fstat(&holding).map_err(|_| ProbeError::Io)?;
            if source_stat.st_dev != holding_stat.st_dev {
                return Err(ProbeError::Unsupported);
            }
            let mut actions = BTreeMap::new();
            for (kind, name, contents) in [
                (RecordKind::NodeIds, "nodeids.json", NODEIDS_BYTES),
                (RecordKind::LastFailed, "lastfailed.json", LASTFAILED_BYTES),
                // The sentinel is created below and is never registered as an action.
            ] {
                let format_kind = match kind {
                    RecordKind::NodeIds => FormatKind::NodeIds,
                    RecordKind::LastFailed => FormatKind::LastFailed,
                };
                classify_pytest_record(format_kind, contents).map_err(|_| ProbeError::Changed)?;
                write_private(&temp.path().join("source").join(name), contents)?;
                let proof = read_proof(&source, name)?;
                actions.insert(
                    kind,
                    ActionState {
                        action_id: ActionId::new(),
                        phase: Phase::Available,
                        receipt: None,
                        proof,
                    },
                );
            }
            write_private(
                &temp.path().join("source").join(SENTINEL_NAME),
                SENTINEL_BYTES,
            )?;
            let ledger =
                approval_ledger::ApprovalLedger::open(temp.path().join("approval-ledger.sqlite3"))
                    .map_err(|_| ProbeError::ApprovalLedger)?;
            Ok(Self {
                _temp: temp,
                temp_parent,
                temp_name,
                root,
                source,
                holding,
                ledger,
                actions,
                reviews: BTreeMap::new(),
                attempts: 0,
                #[cfg(test)]
                fail_finish: false,
                #[cfg(test)]
                last_intent: None,
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(ProbeError::Unsupported)
        }
    }

    pub fn action_id(&self, kind: RecordKind) -> Result<ActionId, ProbeError> {
        self.actions
            .get(&kind)
            .map(|state| state.action_id)
            .ok_or(ProbeError::WrongState)
    }

    /// Read-only check that the committed corpus still matches the strict format detector.
    pub fn record_count(&self, kind: RecordKind) -> Result<u32, ProbeError> {
        if !self.actions.contains_key(&kind) {
            return Err(ProbeError::WrongState);
        }
        let bytes = match kind {
            RecordKind::NodeIds => NODEIDS_BYTES,
            RecordKind::LastFailed => LASTFAILED_BYTES,
        };
        let format_kind = match kind {
            RecordKind::NodeIds => FormatKind::NodeIds,
            RecordKind::LastFailed => FormatKind::LastFailed,
        };
        classify_pytest_record(format_kind, bytes)
            .map(|evidence| evidence.record_count)
            .map_err(|_| ProbeError::Changed)
    }

    pub fn prepare_stage(
        &mut self,
        kind: RecordKind,
        now_ms: i64,
    ) -> Result<ReviewSummary, ProbeError> {
        self.check_review_capacity()?;
        self.verify_bindings()?;
        let state = self.actions.get(&kind).ok_or(ProbeError::WrongState)?;
        if !matches!(state.phase, Phase::Available | Phase::Restored)
            || state.receipt.is_some() && state.phase != Phase::Restored
        {
            return Err(ProbeError::WrongState);
        }
        let current = read_proof(&self.source, source_name(kind))?;
        ensure_same_object(&state.proof, &current)?;
        self.make_review(Operation::Stage, kind, None, current, now_ms)
    }

    pub fn approve_stage(&mut self, review_id: ReviewId, now_ms: i64) -> Result<(), ProbeError> {
        self.approve(review_id, Operation::Stage, now_ms)
    }

    pub fn execute_stage(
        &mut self,
        review_id: ReviewId,
        now_ms: i64,
    ) -> Result<ReceiptId, ProbeError> {
        let review = self.take_approved(review_id, Operation::Stage, now_ms)?;
        let kind = review.summary.kind;
        let state = self
            .actions
            .get(&kind)
            .ok_or(ProbeError::WrongState)?
            .clone();
        if state.action_id != review.summary.action_id
            || state.phase != Phase::Available && state.phase != Phase::Restored
            || state.proof != review.proof
        {
            self.reject_review(review_id);
            return Err(ProbeError::Changed);
        }
        let current = read_proof(&self.source, source_name(kind))?;
        ensure_same_object(&review.proof, &current)?;
        let receipt = review.summary.receipt_id;
        let slot = receipt_slot(receipt)?;
        let execution_id = self.start_intent(&review, now_ms)?;
        match atomic_rename_probe::isolate(&self.source, source_name(kind), &self.holding, &slot) {
            atomic_rename_probe::RenameState::Renamed => {
                let held = match read_proof(&self.holding, &slot) {
                    Ok(proof) => proof,
                    Err(_) => {
                        self.mark_uncertain(kind, execution_id, now_ms);
                        return Err(ProbeError::Uncertain);
                    }
                };
                if !same_content_and_identity(&current, &held) {
                    self.mark_uncertain(kind, execution_id, now_ms);
                    return Err(ProbeError::Uncertain);
                }
                if self
                    .verify_bindings()
                    .and_then(|_| sync_directory(&self.source))
                    .and_then(|_| sync_directory(&self.holding))
                    .is_err()
                    || self.finish_intent(execution_id, now_ms).is_err()
                {
                    self.mark_uncertain(kind, execution_id, now_ms);
                    return Err(ProbeError::Uncertain);
                }
                let Some(state) = self.actions.get_mut(&kind) else {
                    return Err(ProbeError::Uncertain);
                };
                state.phase = Phase::Staged;
                state.receipt = Some(receipt);
                state.proof = held;
                self.mark_review_executed(review_id);
                Ok(receipt)
            }
            atomic_rename_probe::RenameState::DestinationExists => {
                if self.finish_intent(execution_id, now_ms).is_err() {
                    self.mark_uncertain(kind, execution_id, now_ms);
                    return Err(ProbeError::Uncertain);
                }
                self.mark_review_executed(review_id);
                Err(ProbeError::DestinationOccupied)
            }
            atomic_rename_probe::RenameState::Unsupported(_)
            | atomic_rename_probe::RenameState::Failed(_)
            | atomic_rename_probe::RenameState::OutcomeUncertain(_) => {
                self.mark_uncertain(kind, execution_id, now_ms);
                Err(ProbeError::Uncertain)
            }
        }
    }

    pub fn prepare_restore(
        &mut self,
        receipt: ReceiptId,
        now_ms: i64,
    ) -> Result<ReviewSummary, ProbeError> {
        self.check_review_capacity()?;
        self.verify_bindings()?;
        let (kind, state) = self.find_receipt(receipt)?;
        if state.phase != Phase::Staged || state.receipt != Some(receipt) {
            return Err(ProbeError::WrongState);
        }
        let slot = receipt_slot(receipt)?;
        let current = read_proof(&self.holding, &slot)?;
        ensure_same_object(&state.proof, &current)?;
        if named_exists(&self.source, source_name(kind))? {
            return Err(ProbeError::DestinationOccupied);
        }
        self.make_review(Operation::Restore, kind, Some(receipt), current, now_ms)
    }

    pub fn approve_restore(&mut self, review_id: ReviewId, now_ms: i64) -> Result<(), ProbeError> {
        self.approve(review_id, Operation::Restore, now_ms)
    }

    pub fn execute_restore(
        &mut self,
        review_id: ReviewId,
        now_ms: i64,
    ) -> Result<ReceiptId, ProbeError> {
        let review = self.take_approved(review_id, Operation::Restore, now_ms)?;
        let kind = review.summary.kind;
        let receipt = review.summary.receipt_id;
        let state = self
            .actions
            .get(&kind)
            .ok_or(ProbeError::WrongState)?
            .clone();
        if state.phase != Phase::Staged
            || state.receipt != Some(receipt)
            || state.proof != review.proof
        {
            self.reject_review(review_id);
            return Err(ProbeError::Changed);
        }
        let slot = receipt_slot(receipt)?;
        let current = read_proof(&self.holding, &slot)?;
        ensure_same_object(&review.proof, &current)?;
        let execution_id = self.start_intent(&review, now_ms)?;
        match atomic_rename_probe::isolate(&self.holding, &slot, &self.source, source_name(kind)) {
            atomic_rename_probe::RenameState::Renamed => {
                let restored = match read_proof(&self.source, source_name(kind)) {
                    Ok(proof) => proof,
                    Err(_) => {
                        self.mark_uncertain(kind, execution_id, now_ms);
                        return Err(ProbeError::Uncertain);
                    }
                };
                if !same_content_and_identity(&current, &restored) {
                    self.mark_uncertain(kind, execution_id, now_ms);
                    return Err(ProbeError::Uncertain);
                }
                if self
                    .verify_bindings()
                    .and_then(|_| sync_directory(&self.holding))
                    .and_then(|_| sync_directory(&self.source))
                    .is_err()
                    || self.finish_intent(execution_id, now_ms).is_err()
                {
                    self.mark_uncertain(kind, execution_id, now_ms);
                    return Err(ProbeError::Uncertain);
                }
                let Some(state) = self.actions.get_mut(&kind) else {
                    return Err(ProbeError::Uncertain);
                };
                state.phase = Phase::Restored;
                state.receipt = None;
                state.proof = restored;
                self.mark_review_executed(review_id);
                Ok(receipt)
            }
            atomic_rename_probe::RenameState::DestinationExists => {
                if self.finish_intent(execution_id, now_ms).is_err() {
                    self.mark_uncertain(kind, execution_id, now_ms);
                    return Err(ProbeError::Uncertain);
                }
                self.mark_review_executed(review_id);
                Err(ProbeError::DestinationOccupied)
            }
            atomic_rename_probe::RenameState::Unsupported(_)
            | atomic_rename_probe::RenameState::Failed(_)
            | atomic_rename_probe::RenameState::OutcomeUncertain(_) => {
                self.mark_uncertain(kind, execution_id, now_ms);
                Err(ProbeError::Uncertain)
            }
        }
    }

    pub fn prepare_purge(
        &mut self,
        receipt: ReceiptId,
        now_ms: i64,
    ) -> Result<ReviewSummary, ProbeError> {
        self.check_review_capacity()?;
        self.verify_bindings()?;
        let (kind, state) = self.find_receipt(receipt)?;
        if state.phase != Phase::Staged || state.receipt != Some(receipt) {
            return Err(ProbeError::WrongState);
        }
        let slot = receipt_slot(receipt)?;
        let current = read_proof(&self.holding, &slot)?;
        ensure_same_object(&state.proof, &current)?;
        self.make_review(Operation::Purge, kind, Some(receipt), current, now_ms)
    }

    pub fn approve_purge(&mut self, review_id: ReviewId, now_ms: i64) -> Result<(), ProbeError> {
        self.approve(review_id, Operation::Purge, now_ms)
    }

    pub fn execute_purge(
        &mut self,
        review_id: ReviewId,
        now_ms: i64,
    ) -> Result<Outcome, ProbeError> {
        let review = self.take_approved(review_id, Operation::Purge, now_ms)?;
        let kind = review.summary.kind;
        let receipt = review.summary.receipt_id;
        let state = self
            .actions
            .get(&kind)
            .ok_or(ProbeError::WrongState)?
            .clone();
        if state.phase != Phase::Staged
            || state.receipt != Some(receipt)
            || state.proof != review.proof
        {
            self.reject_review(review_id);
            return Err(ProbeError::Changed);
        }
        let slot = receipt_slot(receipt)?;
        let current = read_proof(&self.holding, &slot)?;
        ensure_same_object(&review.proof, &current)?;
        let execution_id = self.start_intent(&review, now_ms)?;
        #[cfg(unix)]
        {
            use rustix::fs::{statat, unlinkat, AtFlags};
            if let Err(error) = unlinkat(&self.holding, slot.as_str(), AtFlags::empty()) {
                self.mark_uncertain(kind, execution_id, now_ms);
                let _ = error;
                return Err(ProbeError::Uncertain);
            }
            match statat(&self.holding, slot.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Err(error) if error == rustix::io::Errno::NOENT => {}
                _ => {
                    self.mark_uncertain(kind, execution_id, now_ms);
                    return Err(ProbeError::Uncertain);
                }
            }
            if self
                .verify_bindings()
                .and_then(|_| sync_directory(&self.holding))
                .is_err()
                || self.finish_intent(execution_id, now_ms).is_err()
            {
                self.mark_uncertain(kind, execution_id, now_ms);
                return Err(ProbeError::Uncertain);
            }
            let Some(state) = self.actions.get_mut(&kind) else {
                return Err(ProbeError::Uncertain);
            };
            state.phase = Phase::PermanentlyRemoved;
            state.receipt = None;
            let action_id = state.action_id;
            self.mark_review_executed(review_id);
            Ok(Outcome::PermanentlyRemoved {
                action_id,
                receipt_id: receipt,
            })
        }
        #[cfg(not(unix))]
        {
            self.mark_uncertain(kind, execution_id, now_ms);
            Err(ProbeError::Unsupported)
        }
    }

    fn make_review(
        &mut self,
        operation: Operation,
        kind: RecordKind,
        old_receipt: Option<ReceiptId>,
        proof: Proof,
        now_ms: i64,
    ) -> Result<ReviewSummary, ProbeError> {
        validate_time(now_ms)?;
        self.check_review_capacity()?;
        let action_id = self
            .actions
            .get(&kind)
            .ok_or(ProbeError::WrongState)?
            .action_id;
        let receipt_id = if operation == Operation::Stage {
            ReceiptId::new()
        } else {
            old_receipt.ok_or(ProbeError::WrongReceipt)?
        };
        let expires_at_unix_ms = now_ms
            .checked_add(REVIEW_TTL_MS)
            .ok_or(ProbeError::InvalidTime)?;
        let review_id = ReviewId::new();
        let approval_id = ApprovalId::new();
        let manifest = Manifest {
            schema: 1,
            adapter_version: "generated-lifecycle-fixture-v1",
            operation: operation_wire(operation),
            action_id: wire(&action_id)?,
            receipt_id: wire(&receipt_id)?,
            kind: kind_wire(kind).into(),
            bytes: proof.stamp.size,
            device: proof.stamp.device,
            inode: proof.stamp.inode,
            mode: proof.stamp.mode,
            links: proof.stamp.links,
            mtime: proof.stamp.mtime,
            mtime_nsec: proof.stamp.mtime_nsec,
            ctime: proof.stamp.ctime,
            ctime_nsec: proof.stamp.ctime_nsec,
            sha256: hex(&proof.sha256),
            created_at_unix_ms: now_ms,
            expires_at_unix_ms,
        };
        let manifest_digest = hex(&Sha256::digest(
            serde_json::to_vec(&manifest).map_err(|_| ProbeError::Io)?,
        ));
        self.ledger
            .insert_approval(&wire(&approval_id)?, &manifest_digest, expires_at_unix_ms)
            .map_err(|_| ProbeError::ApprovalLedger)?;
        let summary = ReviewSummary {
            review_id,
            operation,
            action_id,
            receipt_id,
            kind,
            logical_bytes: proof.stamp.size,
            expires_at_unix_ms,
        };
        self.reviews.insert(
            review_id,
            Review {
                summary: summary.clone(),
                approval_id,
                manifest_digest,
                phase: ReviewPhase::Prepared,
                proof,
                created_at_unix_ms: now_ms,
            },
        );
        Ok(summary)
    }

    fn approve(
        &mut self,
        review_id: ReviewId,
        expected: Operation,
        now_ms: i64,
    ) -> Result<(), ProbeError> {
        validate_time(now_ms)?;
        let review = self
            .reviews
            .get(&review_id)
            .cloned()
            .ok_or(ProbeError::ReviewUnavailable)?;
        if review.summary.operation != expected {
            return Err(ProbeError::WrongOperation);
        }
        if review.phase != ReviewPhase::Prepared {
            return Err(ProbeError::ReviewReused);
        }
        if now_ms < review.created_at_unix_ms {
            return Err(ProbeError::InvalidTime);
        }
        if now_ms >= review.summary.expires_at_unix_ms {
            return Err(ProbeError::Expired);
        }
        self.revalidate_review(&review)?;
        self.ledger
            .consume_approval(&wire(&review.approval_id)?, &review.manifest_digest, now_ms)
            .map_err(|_| ProbeError::ApprovalLedger)?;
        self.reviews
            .get_mut(&review_id)
            .ok_or(ProbeError::ReviewUnavailable)?
            .phase = ReviewPhase::Approved;
        Ok(())
    }

    fn take_approved(
        &mut self,
        review_id: ReviewId,
        expected: Operation,
        now_ms: i64,
    ) -> Result<Review, ProbeError> {
        validate_time(now_ms)?;
        let review = self
            .reviews
            .get(&review_id)
            .cloned()
            .ok_or(ProbeError::ReviewUnavailable)?;
        if review.summary.operation != expected {
            return Err(ProbeError::WrongOperation);
        }
        if review.phase != ReviewPhase::Approved {
            return Err(ProbeError::NotApproved);
        }
        if now_ms < review.created_at_unix_ms {
            return Err(ProbeError::InvalidTime);
        }
        if now_ms >= review.summary.expires_at_unix_ms {
            return Err(ProbeError::Expired);
        }
        self.revalidate_review(&review)?;
        Ok(review)
    }

    fn revalidate_review(&self, review: &Review) -> Result<(), ProbeError> {
        self.verify_bindings()?;
        let state = self
            .actions
            .get(&review.summary.kind)
            .ok_or(ProbeError::WrongState)?;
        if state.action_id != review.summary.action_id || state.phase == Phase::Uncertain {
            return Err(ProbeError::Changed);
        }
        match review.summary.operation {
            Operation::Stage => {
                if state.phase != Phase::Available && state.phase != Phase::Restored {
                    return Err(ProbeError::WrongState);
                }
                let proof = read_proof(&self.source, source_name(review.summary.kind))?;
                ensure_same_object(&review.proof, &proof)
            }
            Operation::Restore | Operation::Purge => {
                if state.phase != Phase::Staged || state.receipt != Some(review.summary.receipt_id)
                {
                    return Err(ProbeError::WrongReceipt);
                }
                let slot = receipt_slot(review.summary.receipt_id)?;
                let proof = read_proof(&self.holding, &slot)?;
                ensure_same_object(&review.proof, &proof)
            }
        }
    }

    #[cfg(unix)]
    fn verify_bindings(&self) -> Result<(), ProbeError> {
        use rustix::fs::{fstat, openat, FileType, Mode, OFlags};
        let root_named = openat(
            &self.temp_parent,
            self.temp_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| ProbeError::Changed)?;
        let root_stat = fstat(&self.root).map_err(|_| ProbeError::Changed)?;
        let root_named_stat = fstat(&root_named).map_err(|_| ProbeError::Changed)?;
        if FileType::from_raw_mode(root_stat.st_mode) != FileType::Directory
            || root_stat.st_dev != root_named_stat.st_dev
            || root_stat.st_ino != root_named_stat.st_ino
        {
            return Err(ProbeError::Changed);
        }
        for (name, held) in [("source", &self.source), ("holding", &self.holding)] {
            let fd = openat(
                &self.root,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| ProbeError::Changed)?;
            let named = fstat(&fd).map_err(|_| ProbeError::Changed)?;
            let held_stat = fstat(held).map_err(|_| ProbeError::Changed)?;
            if FileType::from_raw_mode(named.st_mode) != FileType::Directory
                || named.st_dev != held_stat.st_dev
                || named.st_ino != held_stat.st_ino
                || named.st_dev != root_stat.st_dev
                || named.st_mode & 0o777 != 0o700
            {
                return Err(ProbeError::Changed);
            }
        }
        if root_stat.st_mode & 0o777 != 0o700 {
            return Err(ProbeError::Changed);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn verify_bindings(&self) -> Result<(), ProbeError> {
        Err(ProbeError::Unsupported)
    }

    fn start_intent(&mut self, review: &Review, now_ms: i64) -> Result<ExecutionId, ProbeError> {
        if self.attempts >= MAX_ATTEMPTS {
            return Err(ProbeError::AttemptLimit);
        }
        let execution_id = ExecutionId::new();
        self.ledger
            .record_intent_started(&wire(&execution_id)?, &wire(&review.approval_id)?, now_ms)
            .map_err(|_| ProbeError::ApprovalLedger)?;
        #[cfg(test)]
        {
            self.last_intent = Some(execution_id);
        }
        self.attempts += 1;
        Ok(execution_id)
    }

    fn finish_intent(&mut self, execution_id: ExecutionId, now_ms: i64) -> Result<(), ProbeError> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_finish) {
            return Err(ProbeError::ApprovalLedger);
        }
        self.ledger
            .record_intent_finished(&wire(&execution_id)?, now_ms)
            .map_err(|_| ProbeError::ApprovalLedger)
    }

    fn mark_uncertain(&mut self, kind: RecordKind, execution_id: ExecutionId, now_ms: i64) {
        if let Ok(intent_id) = wire(&execution_id) {
            let _ = self.ledger.record_intent_uncertain(&intent_id, now_ms);
        }
        if let Some(state) = self.actions.get_mut(&kind) {
            state.phase = Phase::Uncertain;
        }
    }

    fn find_receipt(&self, receipt: ReceiptId) -> Result<(RecordKind, ActionState), ProbeError> {
        self.actions
            .iter()
            .find(|(_, state)| state.receipt == Some(receipt))
            .map(|(kind, state)| (*kind, state.clone()))
            .ok_or(ProbeError::WrongReceipt)
    }

    fn check_review_capacity(&self) -> Result<(), ProbeError> {
        if self.reviews.len() >= MAX_REVIEWS {
            Err(ProbeError::ReviewLimit)
        } else {
            Ok(())
        }
    }

    fn reject_review(&mut self, review_id: ReviewId) {
        if let Some(review) = self.reviews.get_mut(&review_id) {
            review.phase = ReviewPhase::Rejected;
        }
    }

    fn mark_review_executed(&mut self, review_id: ReviewId) {
        if let Some(review) = self.reviews.get_mut(&review_id) {
            review.phase = ReviewPhase::Executed;
        }
    }
}

fn validate_time(now_ms: i64) -> Result<(), ProbeError> {
    if now_ms < 0 {
        Err(ProbeError::InvalidTime)
    } else {
        Ok(())
    }
}

fn operation_wire(operation: Operation) -> OperationWire {
    match operation {
        Operation::Stage => OperationWire::Stage,
        Operation::Restore => OperationWire::Restore,
        Operation::Purge => OperationWire::Purge,
    }
}

fn kind_wire(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::NodeIds => "nodeids",
        RecordKind::LastFailed => "lastfailed",
    }
}

fn source_name(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::NodeIds => "nodeids.json",
        RecordKind::LastFailed => "lastfailed.json",
    }
}

fn receipt_slot(receipt: ReceiptId) -> Result<String, ProbeError> {
    Ok(format!(
        "receipt-{}",
        wire(&receipt)?.rsplit(':').next().ok_or(ProbeError::Io)?
    ))
}

fn wire<T: Serialize>(value: &T) -> Result<String, ProbeError> {
    serde_json::to_value(value)
        .map_err(|_| ProbeError::Io)?
        .as_str()
        .map(str::to_owned)
        .ok_or(ProbeError::Io)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn ensure_same_object(expected: &Proof, current: &Proof) -> Result<(), ProbeError> {
    if expected == current {
        Ok(())
    } else {
        Err(ProbeError::Changed)
    }
}

fn same_content_and_identity(left: &Proof, right: &Proof) -> bool {
    left.stamp.device == right.stamp.device
        && left.stamp.inode == right.stamp.inode
        && left.stamp.mode & 0o170000 == right.stamp.mode & 0o170000
        && left.stamp.size == right.stamp.size
        && left.stamp.links == 1
        && right.stamp.links == 1
        && left.sha256 == right.sha256
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), ProbeError> {
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(ProbeError::Changed);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| ProbeError::Io)?;
        file.write_all(bytes).map_err(|_| ProbeError::Io)?;
        file.sync_all().map_err(|_| ProbeError::Io)
    }
    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| ProbeError::Io)?;
        file.write_all(bytes).map_err(|_| ProbeError::Io)?;
        file.sync_all().map_err(|_| ProbeError::Io)
    }
}

// Native stat field widths differ between macOS and Linux.
#[allow(clippy::unnecessary_cast)]
#[cfg(unix)]
fn stat_stamp(stat: &rustix::fs::Stat) -> Result<Stamp, ProbeError> {
    let file_type = rustix::fs::FileType::from_raw_mode(stat.st_mode);
    let size = u64::try_from(stat.st_size).map_err(|_| ProbeError::Changed)?;
    let links = stat.st_nlink as u64;
    if file_type != rustix::fs::FileType::RegularFile || links != 1 || size > MAX_RECORD_BYTES {
        return Err(ProbeError::Changed);
    }
    Ok(Stamp {
        device: stat.st_dev as u64,
        inode: stat.st_ino,
        mode: stat.st_mode as u32,
        size,
        links,
        mtime: stat.st_mtime,
        mtime_nsec: stat.st_mtime_nsec as i64,
        ctime: stat.st_ctime,
        ctime_nsec: stat.st_ctime_nsec as i64,
    })
}

#[cfg(unix)]
fn read_proof(directory: &File, name: &str) -> Result<Proof, ProbeError> {
    use rustix::fs::{fstat, openat, statat, AtFlags, Mode, OFlags};
    let fd = openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| ProbeError::Changed)?;
    let before = fstat(&fd).map_err(|_| ProbeError::Changed)?;
    let stamp = stat_stamp(&before)?;
    let named =
        statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| ProbeError::Changed)?;
    if stat_stamp(&named)? != stamp {
        return Err(ProbeError::Changed);
    }
    let mut file = File::from(fd);
    let mut bytes = Vec::with_capacity(stamp.size as usize);
    (&mut file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ProbeError::Changed)?;
    if bytes.len() as u64 != stamp.size {
        return Err(ProbeError::Changed);
    }
    let after = fstat(&file).map_err(|_| ProbeError::Changed)?;
    let after_stamp = stat_stamp(&after)?;
    let named_after =
        statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| ProbeError::Changed)?;
    if after_stamp != stamp || stat_stamp(&named_after)? != stamp {
        return Err(ProbeError::Changed);
    }
    Ok(Proof {
        stamp,
        sha256: Sha256::digest(&bytes).into(),
    })
}

#[cfg(not(unix))]
fn read_proof(_: &File, _: &str) -> Result<Proof, ProbeError> {
    Err(ProbeError::Unsupported)
}

fn named_exists(directory: &File, name: &str) -> Result<bool, ProbeError> {
    #[cfg(unix)]
    {
        match rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(error) if error == rustix::io::Errno::NOENT => Ok(false),
            Err(_) => Err(ProbeError::Io),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, name);
        Err(ProbeError::Unsupported)
    }
}

fn sync_directory(directory: &File) -> Result<(), ProbeError> {
    directory.sync_all().map_err(|_| ProbeError::Io)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    const T0: i64 = 10_000;

    fn approve_and_stage(
        probe: &mut GeneratedLifecycleProbe,
        kind: RecordKind,
        now: i64,
    ) -> ReceiptId {
        let review = probe.prepare_stage(kind, now).unwrap();
        probe.approve_stage(review.review_id(), now + 1).unwrap();
        probe.execute_stage(review.review_id(), now + 2).unwrap()
    }

    #[test]
    fn corpus_is_recognized_then_staged_restored_staged_and_purged_with_distinct_reviews() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        assert!(probe.record_count(RecordKind::NodeIds).unwrap() > 0);
        assert!(probe.record_count(RecordKind::LastFailed).unwrap() > 0);
        let lastfailed_before =
            fs::read(probe._temp.path().join("source/lastfailed.json")).unwrap();
        let notes_before = fs::read(probe._temp.path().join("source").join(SENTINEL_NAME)).unwrap();
        let original = fs::read(probe._temp.path().join("source/nodeids.json")).unwrap();
        let original_proof = probe
            .actions
            .get(&RecordKind::NodeIds)
            .unwrap()
            .proof
            .clone();
        let first = approve_and_stage(&mut probe, RecordKind::NodeIds, T0);
        let staged_proof = probe
            .actions
            .get(&RecordKind::NodeIds)
            .unwrap()
            .proof
            .clone();
        assert_eq!(staged_proof.sha256, original_proof.sha256);
        assert_eq!(staged_proof.stamp.device, original_proof.stamp.device);
        assert_eq!(staged_proof.stamp.inode, original_proof.stamp.inode);
        let restore = probe.prepare_restore(first, T0 + 3).unwrap();
        probe.approve_restore(restore.review_id(), T0 + 4).unwrap();
        assert_eq!(
            probe.execute_restore(restore.review_id(), T0 + 5),
            Ok(first)
        );
        let restored_proof = probe
            .actions
            .get(&RecordKind::NodeIds)
            .unwrap()
            .proof
            .clone();
        assert_eq!(restored_proof.sha256, original_proof.sha256);
        assert_eq!(restored_proof.stamp.device, original_proof.stamp.device);
        assert_eq!(restored_proof.stamp.inode, original_proof.stamp.inode);
        assert_eq!(
            fs::read(probe._temp.path().join("source/nodeids.json")).unwrap(),
            original
        );
        let second = approve_and_stage(&mut probe, RecordKind::NodeIds, T0 + 6);
        assert_ne!(first, second);
        let purge = probe.prepare_purge(second, T0 + 9).unwrap();
        assert_ne!(purge.review_id(), restore.review_id());
        probe.approve_purge(purge.review_id(), T0 + 10).unwrap();
        assert!(
            matches!(probe.execute_purge(purge.review_id(), T0 + 11), Ok(Outcome::PermanentlyRemoved { receipt_id, .. }) if receipt_id == second)
        );
        assert!(!probe
            ._temp
            .path()
            .join(format!("holding/{}", receipt_slot(second).unwrap()))
            .exists());
        assert_eq!(
            fs::read(probe._temp.path().join("source/lastfailed.json")).unwrap(),
            lastfailed_before
        );
        assert_eq!(
            fs::read(probe._temp.path().join("source").join(SENTINEL_NAME)).unwrap(),
            notes_before
        );
        assert_eq!(
            probe.prepare_purge(second, T0 + 12),
            Err(ProbeError::WrongReceipt)
        );
    }

    #[test]
    fn refuses_changed_source_before_stage_and_never_stages_unknown_sentinel() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        fs::write(probe._temp.path().join("source/nodeids.json"), b"changed").unwrap();
        assert_eq!(
            probe.prepare_stage(RecordKind::NodeIds, T0),
            Err(ProbeError::Changed)
        );
        assert!(probe.actions.contains_key(&RecordKind::NodeIds));
        assert_eq!(
            fs::read(probe._temp.path().join("source").join(SENTINEL_NAME)).unwrap(),
            SENTINEL_BYTES
        );
    }

    #[test]
    fn restore_refuses_original_name_collision_without_overwriting_it() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        let receipt = approve_and_stage(&mut probe, RecordKind::NodeIds, T0);
        let collision = b"valuable collision";
        fs::write(probe._temp.path().join("source/nodeids.json"), collision).unwrap();
        assert_eq!(
            probe.prepare_restore(receipt, T0 + 3),
            Err(ProbeError::DestinationOccupied)
        );
        assert_eq!(
            fs::read(probe._temp.path().join("source/nodeids.json")).unwrap(),
            collision
        );
        assert!(probe
            ._temp
            .path()
            .join(format!("holding/{}", receipt_slot(receipt).unwrap()))
            .exists());
    }

    #[test]
    fn stage_refuses_holding_name_collision_without_moving_source() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        let review = probe.prepare_stage(RecordKind::NodeIds, T0).unwrap();
        let slot = receipt_slot(review.receipt_id()).unwrap();
        let collision = b"holding collision";
        fs::write(probe._temp.path().join("holding").join(&slot), collision).unwrap();
        probe.approve_stage(review.review_id(), T0 + 1).unwrap();
        assert_eq!(
            probe.execute_stage(review.review_id(), T0 + 2),
            Err(ProbeError::DestinationOccupied)
        );
        assert_eq!(
            fs::read(probe._temp.path().join("source/nodeids.json")).unwrap(),
            NODEIDS_BYTES
        );
        assert_eq!(
            fs::read(probe._temp.path().join("holding").join(slot)).unwrap(),
            collision
        );
    }

    #[test]
    fn wrong_receipt_expired_review_and_reused_review_are_refused() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        assert_eq!(
            probe.prepare_restore(ReceiptId::new(), T0),
            Err(ProbeError::WrongReceipt)
        );
        let review = probe.prepare_stage(RecordKind::NodeIds, T0).unwrap();
        probe.approve_stage(review.review_id(), T0 + 1).unwrap();
        assert_eq!(
            probe.execute_stage(review.review_id(), review.expires_at_unix_ms()),
            Err(ProbeError::Expired)
        );
        let retry = probe.prepare_stage(RecordKind::NodeIds, T0 + 20).unwrap();
        probe.approve_stage(retry.review_id(), T0 + 21).unwrap();
        let receipt = probe.execute_stage(retry.review_id(), T0 + 22).unwrap();
        assert_eq!(
            probe.execute_stage(retry.review_id(), T0 + 23),
            Err(ProbeError::NotApproved)
        );
        assert_eq!(
            probe.prepare_restore(receipt, T0 + 24).unwrap().operation(),
            Operation::Restore
        );
    }

    #[test]
    fn changed_held_record_blocks_purge() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        let receipt = approve_and_stage(&mut probe, RecordKind::NodeIds, T0);
        let slot = receipt_slot(receipt).unwrap();
        fs::write(
            probe._temp.path().join("holding").join(slot),
            b"changed while held",
        )
        .unwrap();
        assert_eq!(
            probe.prepare_purge(receipt, T0 + 3),
            Err(ProbeError::Changed)
        );
    }

    #[test]
    fn review_and_attempt_caps_refuse_before_mutation() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        for n in 0..MAX_REVIEWS {
            probe
                .prepare_stage(RecordKind::NodeIds, T0 + n as i64 * 2)
                .unwrap();
        }
        let before = fs::read(probe._temp.path().join("source/nodeids.json")).unwrap();
        assert_eq!(
            probe.prepare_stage(RecordKind::NodeIds, T0 + 1000),
            Err(ProbeError::ReviewLimit)
        );
        assert_eq!(
            fs::read(probe._temp.path().join("source/nodeids.json")).unwrap(),
            before
        );

        let mut attempts = GeneratedLifecycleProbe::create().unwrap();
        let review = attempts.prepare_stage(RecordKind::NodeIds, T0).unwrap();
        attempts.approve_stage(review.review_id(), T0 + 1).unwrap();
        attempts.attempts = MAX_ATTEMPTS;
        assert_eq!(
            attempts.execute_stage(review.review_id(), T0 + 2),
            Err(ProbeError::AttemptLimit)
        );
        assert!(attempts._temp.path().join("source/nodeids.json").exists());
    }
    #[test]
    fn terminal_journal_failure_after_stage_blocks_all_retries_and_persists_uncertainty() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        let review = probe.prepare_stage(RecordKind::NodeIds, T0).unwrap();
        probe.approve_stage(review.review_id(), T0 + 1).unwrap();
        probe.fail_finish = true;
        assert_eq!(
            probe.execute_stage(review.review_id(), T0 + 2),
            Err(ProbeError::Uncertain)
        );
        assert!(!probe._temp.path().join("source/nodeids.json").exists());
        assert!(probe
            ._temp
            .path()
            .join("holding")
            .join(receipt_slot(review.receipt_id()).unwrap())
            .exists());
        assert_eq!(probe.actions[&RecordKind::NodeIds].phase, Phase::Uncertain);
        assert_eq!(
            probe.prepare_stage(RecordKind::NodeIds, T0 + 3),
            Err(ProbeError::WrongState)
        );
        assert_eq!(
            probe.execute_stage(review.review_id(), T0 + 3),
            Err(ProbeError::Changed)
        );
        let intent_id = wire(&probe.last_intent.unwrap()).unwrap();
        // Reopen durable bookkeeping without reconstructing or retrying an action.
        let reopened = approval_ledger::ApprovalLedger::open(
            probe._temp.path().join("approval-ledger.sqlite3"),
        )
        .unwrap();
        assert_eq!(
            reopened.execution_intent(&intent_id).unwrap().state,
            approval_ledger::IntentState::Uncertain
        );
    }

    #[test]
    fn restore_collision_after_approval_preserves_both_files() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        let receipt = approve_and_stage(&mut probe, RecordKind::NodeIds, T0);
        let restore = probe.prepare_restore(receipt, T0 + 3).unwrap();
        probe.approve_restore(restore.review_id(), T0 + 4).unwrap();
        fs::write(
            probe._temp.path().join("source/nodeids.json"),
            b"new valuable file",
        )
        .unwrap();
        assert_eq!(
            probe.execute_restore(restore.review_id(), T0 + 5),
            Err(ProbeError::DestinationOccupied)
        );
        assert_eq!(
            fs::read(probe._temp.path().join("source/nodeids.json")).unwrap(),
            b"new valuable file"
        );
        assert_eq!(
            fs::read(
                probe
                    ._temp
                    .path()
                    .join("holding")
                    .join(receipt_slot(receipt).unwrap())
            )
            .unwrap(),
            NODEIDS_BYTES
        );
    }

    #[test]
    fn operation_approval_and_clock_cannot_be_substituted() {
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        let review = probe.prepare_stage(RecordKind::NodeIds, T0).unwrap();
        assert_eq!(
            probe.execute_stage(review.review_id(), T0 + 1),
            Err(ProbeError::NotApproved)
        );
        assert_eq!(
            probe.approve_purge(review.review_id(), T0 + 1),
            Err(ProbeError::WrongOperation)
        );
        assert_eq!(
            probe.approve_stage(review.review_id(), T0 - 1),
            Err(ProbeError::InvalidTime)
        );
        assert_eq!(
            probe.approve_stage(review.review_id(), review.expires_at_unix_ms()),
            Err(ProbeError::Expired)
        );
        assert!(probe._temp.path().join("source/nodeids.json").exists());
    }

    #[test]
    fn replaced_source_directory_and_permission_changes_refuse() {
        use std::os::unix::fs::PermissionsExt;
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        let path = probe._temp.path().join("source/nodeids.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(
            probe.prepare_stage(RecordKind::NodeIds, T0),
            Err(ProbeError::Changed)
        );
        let mut probe = GeneratedLifecycleProbe::create().unwrap();
        fs::rename(
            probe._temp.path().join("source"),
            probe._temp.path().join("old-source"),
        )
        .unwrap();
        fs::create_dir(probe._temp.path().join("source")).unwrap();
        assert_eq!(
            probe.prepare_stage(RecordKind::NodeIds, T0),
            Err(ProbeError::Changed)
        );
        assert_eq!(
            fs::read(probe._temp.path().join("old-source/nodeids.json")).unwrap(),
            NODEIDS_BYTES
        );
    }
    #[test]
    fn terminal_journal_failure_after_restore_or_purge_never_allows_retry() {
        for operation in [Operation::Restore, Operation::Purge] {
            let mut probe = GeneratedLifecycleProbe::create().unwrap();
            let receipt = approve_and_stage(&mut probe, RecordKind::NodeIds, T0);
            let review = match operation {
                Operation::Restore => probe.prepare_restore(receipt, T0 + 3).unwrap(),
                Operation::Purge => probe.prepare_purge(receipt, T0 + 3).unwrap(),
                _ => unreachable!(),
            };
            probe
                .approve(review.review_id(), operation, T0 + 4)
                .unwrap();
            probe.fail_finish = true;
            match operation {
                Operation::Restore => assert_eq!(
                    probe.execute_restore(review.review_id(), T0 + 5),
                    Err(ProbeError::Uncertain)
                ),
                Operation::Purge => assert_eq!(
                    probe.execute_purge(review.review_id(), T0 + 5),
                    Err(ProbeError::Uncertain)
                ),
                _ => unreachable!(),
            }
            assert_eq!(probe.actions[&RecordKind::NodeIds].phase, Phase::Uncertain);
            assert_eq!(
                probe.prepare_purge(receipt, T0 + 6),
                Err(ProbeError::WrongState)
            );
            assert_eq!(
                probe.prepare_restore(receipt, T0 + 6),
                Err(ProbeError::WrongState)
            );
            assert_eq!(
                probe
                    .ledger
                    .execution_intent(&wire(&probe.last_intent.unwrap()).unwrap())
                    .unwrap()
                    .state,
                approval_ledger::IntentState::Uncertain
            );
            assert_eq!(
                probe._temp.path().join("source/nodeids.json").exists(),
                operation == Operation::Restore
            );
        }
    }
}
