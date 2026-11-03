# Disposable fixture purge contract v1

**Status: batches A–E implemented for app-created disposable fixtures.** Receipt-based lifecycle purge, prepared reviews and single-use claims, approval-ledger consumption, native confirmation, cross-store uncertainty consultation, logical-byte accounting, subprocess recovery tests, and the demo UI are implemented. Permanent removal still requires a separate exact review and native confirmation. Controlled-volume measurement remains open; logical bytes unlinked are not measured free space. M0 and M1B human pilot gates remain open, and no user-file cleanup or supported release is enabled. See [M1C validation](../M1C-VALIDATION.md) for evidence and limitations.

## Scope and API boundary

The original service entry point accepts only `FixtureProfile::SyntheticV1`. Preparation, revalidation and claimed batch execution reject the closed pytest corpus profile before claiming a review or touching files. A separate backend corpus entry point uses a distinct control identity, exact two-record plan, review kind and manifest adapter version; see [the corpus execution contract](pytest-corpus-execution-v1.md). Synthetic reviews and adapter versions remain unchanged and cannot authorize corpus records.

Purge may target only one to five exact `(ActionId, ReceiptId)` pairs for one `SessionId`, where each receipt is currently staged in that session. The caller selects every target explicitly. There is no “all staged” default, path input, recursive operation, timer, Trash access, arbitrary file support, or renderer authority. Every selected action must also be `Include` in the current five-action fixture example plan (`review-plan::ReviewPlanStore`). The general inventory editor and its metadata-only reviews remain non-executable and cannot authorize fixture operations. The prepared review freezes the exact plan, included actions, session, receipt identities, and purge targets; preparation fails if any selected action is absent, excluded, stale, duplicated, or not staged.

Proposed trusted Rust API:

```rust
pub struct PurgeTarget {
    pub action_id: ActionId,
    pub receipt_id: ReceiptId,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PurgeReceiptState {
    Staged,
    PurgeIntent,
    PermanentlyRemoved,
    Uncertain,
}

pub enum PurgeItemOutcome {
    NotAttempted { receipt_id: ReceiptId },
    PermanentlyRemoved { receipt_id: ReceiptId, logical_bytes: u64 },
    Uncertain { receipt_id: ReceiptId },
}

pub struct PurgeReviewTarget {
    pub action_id: ActionId,
    pub receipt_id: ReceiptId,
    pub label: String,
    pub logical_bytes: u64,
    pub status: PurgeReceiptState,
}

pub struct PreparedPurgeReview {
    pub schema_version: u32,
    pub kind: String, // "disposable_fixture_purge_review"
    pub review_id: ReviewId,
    pub plan_id: PlanId,
    pub revision_id: RevisionId,
    pub session_id: SessionId,
    pub operation: SupportedOperation, // must be PermanentlyRemoveStaged
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub manifest_sha256: String,
    pub targets: Vec<PurgeReviewTarget>, // 1..=5; explicit exact pairs
    pub selected_logical_bytes: u64,
}

impl FixturePurgeService {
    pub fn prepare_purge_review(
        &mut self,
        expected_revision: RevisionId,
        targets: Vec<PurgeTarget>,
    ) -> Result<PreparedPurgeReview, PurgeError>;
}

impl FixtureLifecycle {
    pub fn purge_receipt(
        &mut self,
        receipt_id: ReceiptId,
        attempt_id: AttemptId,
    ) -> Result<PurgeItemOutcome, PurgeError>;
}
```

These are internal backend contracts. No renderer-facing edit or command accepts an `approved: bool`, paths, a manifest, a raw identity, or an `Approval`. Native confirmation is supplied through the trusted coordinator callback, as in disposable execution v1. IDs are references, never bearer permissions. `purge_receipt` resolves a receipt only within the open session and never accepts a target path.

`PurgeReviewTarget` is a sanitized public summary: action ID, receipt ID, fixed fixture label, logical bytes, and status. It contains no filesystem path, inode, raw device identity, or content. The private manifest binds every exact target to the fixture's registered device/inode, byte length, content digest, directory identities, receipt slot, plan/revision, session, operation, adapter/schema versions, selected byte sum, and review timestamps. Canonical serialization and a fixed hash cover the complete manifest. SQL byte limits are checked before JSON is loaded.

Preparation and revalidation use the same target builder: each selected action and receipt resolve to that exact registered fixture, regardless of selection order or position in the full registry. A non-prefix subset must not borrow another fixture’s size or hashes. Historical reviews with mismatching manifests remain refused; they are not rewritten or revived. A wall-clock reversal observed across the trusted confirmation callback refuses before the execution claim.

## Review, confirmation, and durable authority

The coordinator holds the existing exclusive app-owner lease from open through confirmation, execution, and recovery. It prepares an immutable review with a short bounded lifetime (five minutes by default); expiry is fixed at creation and independently indexed. Clock reversal, expiry, changed review bytes, reuse, unsupported schema, or a stale plan revision refuses the operation. Historical plan reviews and approval ledger rows remain immutable and are never reset or rewritten.

The native dialog is parented to the application and states that the selected staged examples will be permanently removed, lists each exact label and logical size, states that this cannot be undone through the app, and explains that logical bytes are not a promise of free-space recovery. Cancel performs no fixture mutation and issues no execution approval. The coordinator revalidates current plan revision, include choices, session and directory bindings, selected receipt state, content identity, review digest and expiry immediately before opening the dialog and again after it returns. Confirmation applies only to the frozen target list.

After accepted confirmation and fresh revalidation, the coordinator atomically claims the unused review and persists a unique batch execution ID plus child attempt IDs before file mutation. The claim compares the complete prepared review with the durable row under the write transaction, including its time window. Before a batch touches any receipt, its execution ID, review/session bindings and exact complete pending child-attempt mapping must match the stored claim; missing, extra, reordered, substituted or replayed attempts refuse. Completed batches finalize their approval-ledger intent. A batch error or terminal ledger failure records durable uncertainty and blocks further work while preserving already known permanent-removal outcomes. It reconstructs and validates a fresh typed core approval for the exact frozen plan/revision and manifest digest, consumes that approval once through the existing `ApprovalLedger`, and durably records execution intent. If any step fails, no unlink occurs. If the process stops between the ledger transaction and control-store intent, the approval remains consumed and the run requires inspection; it is never replayed. SQLite databases use exact known schemas, bounded values, no-follow leaf opens, private app-owned parents, and fail-closed migrations. The existing approval ledger and plan-store schemas are preserved. The fixture lifecycle requires an allowlisted v1-to-v2 migration; the separate purge control database has its own fixed application ID and schema version.

## Filesystem and item state transitions

Each item is processed serially by its registered receipt, not by a path supplied by the caller. It must be a completed stage operation using the existing same-volume, no-overwrite rename into the app's private holding directory. Before removal, the backend opens trusted root/holding directory handles and verifies their recorded identities and privacy. It resolves only the fixed registered receipt slot relative to the holding directory without following symlinks; it verifies a regular file, expected device/inode, one link, exact size, and content digest. The source and holding locations must remain on the expected volume. Missing files, replaced directories, hard links, symlinks, changed contents, unsupported filesystems, or occupied/untrusted locations stop the batch without guessing.

The backend then calls a directory-relative unlink for the registered leaf and syncs the holding directory. Directory-relative operations bind ancestor resolution to the opened holding directory. They do not atomically bind the leaf unlink to the verified inode; portable filesystems do not provide an atomic “unlink only if this name still has this inode” primitive. A same-user process able to mutate the private fixture directory can race the final verification and unlink. The service must retain restrictive directory ownership/mode and open directory handles, recheck immediately before unlink, document this residual same-user TOCTOU, and report uncertainty if the result cannot be established. It must not claim conditional-unlink guarantees.

Suggested typed item outcomes and durable receipt states:

- `PurgeItemOutcome::NotAttempted`: no unlink intent was claimed for this item; this batch does not change its stored receipt state. Fresh validation is still required before offering restore, because external changes may already have made the target unavailable or invalid.
- `PurgeItemOutcome::PermanentlyRemoved`: unlink and directory sync completed and the terminal result was durably recorded; its receipt state is `PermanentlyRemoved` and it can never be restored.
- `PurgeItemOutcome::Uncertain`: unlink may or may not have occurred, or the result could not be durably committed; its receipt state is `Uncertain` and automatic purge retry and restore are blocked until inspection resolves the outcome.

Persist an item intent before unlink and a terminal outcome after unlink and directory sync. A crash with an intent but no durable outcome becomes `uncertain` on later owner recovery. Recovery reads only exact journal rows and registered slots; it never retries the unlink, scans directories recursively, or deletes on expiry. A later fresh review is required for any new action. Known `permanently_removed` is the only state that permanently invalidates restore; unknown or uncertain outcomes are not reported as successful removal.

## Accounting

`selectedLogicalBytes` is the sum of registered fixture lengths selected for review. `durablyUnlinkedLogicalBytes` counts only files whose unlink, holding-directory sync, and durable terminal outcome are known complete. It is not measured free space.

An optional separate result may report `observedFreeSpaceDeltaBytes: Option<i64>` as a signed observation from before/after measurements on the controlled same volume. It requires an exclusive controlled-volume pilot and platform-specific measurement validation. Other writers, open handles, APFS snapshots, clones, compression, filesystem metadata, and delayed reclamation can change the observed delta. The result must include measurement method and volume binding and must never attribute the entire delta to this operation without controlled evidence. Missing or unreliable measurements return `None`; they do not fail or change the logical unlink result.

Implemented wire extension at 0.0.16: `PurgeExecutionResult.volumeObservation` is omitted when unavailable. When present it contains `observedFreeSpaceDeltaBytes` (signed, after minus before), `method` (`fstatvfs_bavail_v1`) and `volumeBindingSha256` (an opaque per-execution binding). These are ephemeral immediate-response observations, not persisted journal facts or approval inputs. The macOS adapter samples the verified opened holding-directory handle after confirmation, revalidation and authority checks, then after the durable batch. Only finished results may include an observation. Failed readings, mismatched bindings, integer overflow or values outside JavaScript's exact integer range omit it. No sleeps, retries, eligibility changes, renderer-supplied paths or new measurement commands are added. Linux currently returns no observation. UI wording states that the volume-wide change is not attributed to individual files.


## Storage and migration

The fixture lifecycle schema v2 retains the v1 application ID (`0x4453464c`), manifest, fixture, receipt, and attempt records and adds the reviewed purge operation and terminal vocabulary only. Migration must use exact v1 DDL, object set, schema version, and row validation; preserve every old row, identity, receipt, status, attempt order, and rowid atomically. Unknown schemas, malformed or oversized rows, future versions, symlinks, and unknown objects remain untouched and are refused. Faults before commit roll back schema and data. An isolated migration probe is supporting evidence only; it does not authorize production migration. The proposed separate control database is `fixture-purge-control-v1.sqlite3`, with a fixed application ID and `user_version = 1`; it owns review claims, batch execution, child attempts, and outcomes. It must be opened and used by the existing lease-owning coordinator, not a second independently leased service. Stage, restore and purge must consult uncertainty across both control stores. Existing demo-control v1 rows remain intact. This storage split requires implementation review before it is frozen. Neither store is reset or implicitly migrated from unknown data.

Sanitized stable error codes should distinguish stale revision, expired/replayed review, confirmation declined, changed target, collision/untrusted directory, unsupported schema, busy owner, ledger unavailable, storage failure, and uncertain outcome. Errors and public DTOs never reveal raw paths or SQL text.

## Required evidence before enabling

- Unit tests cover exact selection and include matching; 1–5 target bounds; duplicate/mismatched receipts; review hash, expiry, stale revision, replay, and historical-review preservation; byte bounds; and each status transition.
- Filesystem tests use only fresh disposable fixtures and sentinels. They cover missing/replaced parents, symlink and hard-link substitution, changed size/content, name collisions, directory privacy, volume mismatch, and failure at each verify/unlink/sync/commit boundary.
- Subprocess tests terminate between review claim, ledger consumption, execution intent, unlink, directory sync, and outcome persistence. Reopen proves no automatic retry, replay, restore-after-known-removal, or success claim for uncertain items.
- Native integration tests verify the parented exact-target warning, cancellation with no filesystem mutation, fresh revalidation after callback, serialized ownership, and structured results without paths.
- A controlled-volume pilot validates observed free-space reporting separately from logical bytes. M0 and M1B human pilots must pass before product activation; these synthetic tests do not satisfy those gates.
