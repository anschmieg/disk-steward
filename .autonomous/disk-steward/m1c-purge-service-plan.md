# Purge service (Batch B) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (native, established; ledger = `.autonomous/disk-steward/progress.md`). Steps use checkbox (`- [ ]`) syntax.

**Goal:** Implement `FixturePurgeService` — the review/claim/execute coordinator for receipt-addressed fixture purge, with its own fail-closed control store — so purge becomes invokable end-to-end *within the crate*, while approval-ledger consumption, native confirmation wiring, and demo-execution integration remain Batch C.

**Architecture:** New crate `crates/fixture-purge`. The service is constructed by the lease-owning coordinator (it takes already-opened `FixtureLifecycle` + `ReviewPlanStore` handles and owns **no lease of its own**, per contract). `prepare_purge_review` freezes plan revision, Include decisions, session, staged receipt identities and exact targets into a private manifest (canonical typed JSON, fixed SHA-256) stored in `fixture-purge-control-v1.sqlite3` (application id, `user_version = 1`, STRICT, exact-object validation, no-follow open, fail-closed on unknown data, never reset). `claim_and_execute` revalidates everything fresh, calls the trusted confirmer callback, atomically claims the unused review (execution row + pending child-attempt rows before any file mutation), then serially calls `FixtureLifecycle::purge_receipt` per target — any refusal stops the batch and the remaining items record `NotAttempted`. Recovery on open marks a claimed-but-unfinished execution and its pending child rows `Uncertain`; nothing retries, replays, or deletes on expiry.

**Tech Stack:** Rust 2021, rusqlite (bundled, IMMEDIATE transactions, WAL checkpoint), serde snake_case DTOs, sha2; reuses `SupportedOperation::PermanentlyRemoveStaged` (core), `PurgeItemOutcome`/`FixtureStatus` (fixture-lifecycle), `PlanView`/`Decision`/`StoreError` (review-plan).

**Spec:** `docs/contracts/fixture-purge-v1.md` (batch A section: lifecycle layer; this batch implements the service/control-store sections minus ledger/confirmation wiring).

## Global Constraints

- The caller selects every target explicitly: 1..=5 exact `(ActionId, ReceiptId)` pairs, no duplicates by action or receipt, no "all staged" default, no paths, no recursive operation, no timer, no Trash.
- Every selected action must be `Decision::Include` in the current plan revision; the revision must equal `expected_revision` (else `StaleRevision`); the general inventory editor cannot authorize fixtures.
- Every selected receipt must be currently `Staged` in the session; anything else (available/restored/changed/uncertain/permanently-removed) refuses with `ChangedTarget` semantics before any row is written.
- Reviews are single-use, short-lived (five minutes by default, fixed at creation, independently indexed), replay-refused; expiry and re-expiry are checked before and after the confirmation callback.
- Cancel/decline performs no fixture mutation, consumes nothing, and leaves the review prepared until expiry.
- Child attempt IDs are persisted before file mutation; per-item intents live in the lifecycle store (already durable), batch outcomes in the control store.
- Errors are sanitized typed codes, never paths or SQL text. `PurgeError` covers: stale revision, expired/replayed review, confirmation declined, changed target, unsupported schema, busy owner, storage failure, uncertain outcome, ledger unavailable (Batch C), invalid input.
- Recovery reads only exact journal rows; it never retries the unlink, scans directories, or deletes on expiry.
- Tests: disposable tempdir fixtures only; unix-only; versions 0.0.11; no release; PR targets `feat/m1c-purge-schema`. No demo-execution/src-tauri/UI changes in this batch.

## Review Focus

- **A claimed execution that crashes before item outcomes** must reopen as `Uncertain`, refuse replay, and never report success for pending items (Task 5 pins this).
- **A fabricated review row** (hand-written control DB) must fail validation on open — exact-schema, exact-application-id and bounded-value validation is the only barrier (Task 1 pins the schema layer; the review row's manifest hash binds its content, Task 3).
- **Expiry racing the confirmation callback** (expires between revalidation and callback return): the post-callback expiry recheck must refuse (Task 4 pins, mirroring demo-execution).
- **A target whose receipt state changed between prepare and claim** (restored elsewhere, file tampered) must refuse the claim before any mutation (Task 4).
- **The manifest must not contain paths, inodes, or raw device ids** — identity digests bind them transitively; the sanitized `PurgeReviewTarget` carries label/logical bytes/status only (Task 3 pins the wire shape).

---

### Task 1: Control store schema and fail-closed open

**Files:** Create `crates/fixture-purge/src/lib.rs` (types + store).

- [ ] **Step 1: Failing tests:** open creates the DB with exact application id and `user_version=1`; three STRICT tables (`purge_reviews`, `purge_executions`, `purge_child_attempts`); extra table/object → open refuses `PurgeError::UnsupportedSchema`; foreign `user_version` → refuses, never resets; reopen is idempotent (no reseed, rows preserved); WAL mode + synchronous=FULL.
- [ ] **Step 2:** Run → FAIL (crate empty). **Step 3:** Implement schema constants, initialize, exact-object validation, no-follow open with private-parent checks. **Step 4:** Run → PASS. **Step 5:** Commit `feat: fail-closed purge control store`.

### Task 2: Review and error types

**Files:** same crate.

- [ ] **Step 1: Failing tests:** serde wire shapes (`PurgeReviewTarget` snake_case, label + logical bytes + status; `PreparedPurgeReview` with kind `"disposable_fixture_purge_review"`, operation serialized as `permanently_remove_staged`); `PurgeError::code()` returns the sanitized codes from the contract list.
- [ ] **Step 2-5:** implement types (`PurgeTarget`, `PurgeReceiptState`, `PurgeReviewTarget`, `PreparedPurgeReview`, `PurgeItemBatchOutcome`, `PurgeExecutionResult`, `PurgeError`) → PASS → Commit `feat: purge review and outcome types`.

### Task 3: `prepare_purge_review`

**Files:** same crate.

**Interfaces:** `pub fn prepare_purge_review(&mut self, expected_revision: RevisionId, targets: Vec<PurgeTarget>) -> Result<PreparedPurgeReview, PurgeError>`; service constructor `open(root: &Path, lifecycle: FixtureLifecycle, plan: ReviewPlanStore)` + `open_with_clock(..., now: Box<dyn Fn() -> i64 + Send + Sync>)`.

- [ ] **Step 1: Failing tests (each its own assertion set):** happy path (3 staged targets → review persisted, manifest hash stable across two preparations of identical state, expiry = created+300s); empty targets refused; six targets refused; duplicate action refused; duplicate receipt refused; unknown action refused; `Keep` action refused; stale revision refused; receipt not staged refused; targets JSON byte-bounded.
- [ ] **Step 2:** RED. **Step 3:** implement (plan revision + decisions from `get_plan()`; staged states from `lifecycle.snapshot()`; manifest = canonical typed JSON of session/revision/plan/targets' registered digests + adapter/schema versions + timestamps + selected byte sum; fixed hash; `INSERT` bounded row). **Step 4:** PASS. **Step 5:** Commit `feat: prepare immutable purge reviews`.

### Task 4: `claim_and_execute`

**Files:** same crate.

**Interfaces:** `pub fn claim_and_execute(&mut self, review_id: ReviewId, confirmer: impl FnOnce(&PreparedPurgeReview) -> bool) -> Result<PurgeExecutionResult, PurgeError>`.

- [ ] **Step 1: Failing tests:** happy 3-target purge (files gone from holding; lifecycle states `PermanentlyRemoved`; control rows terminal `permanently_removed`; result `finished` with three `PermanentlyRemoved` outcomes + selected/durably-unlinked logical bytes); declined callback → `ConfirmationDeclined`, review still prepared, files intact; changed target before claim → refused, nothing mutated; expired → refused; post-callback expiry (injectable clock advanced inside the callback) → refused, no mutation; replay of a claimed/finished review → refused; mid-batch stop via callback-tampered second target (callback runs after revalidation, before claim) → item 1 `PermanentlyRemoved`, items 2-3 `NotAttempted`, execution `partial`, remaining child rows `not_attempted`.
- [ ] **Step 2:** RED. **Step 3:** implement (revalidate fresh: revision, includes, staged states, manifest hash, expiry; callback; re-expiry; atomic claim tx: review `prepared`→`claimed` + execution row + pending child rows; serial `purge_receipt`; map outcomes; stop on refusal; terminal journal updates; execution `finished`/`partial`). **Step 4:** PASS. **Step 5:** Commit `feat: claim and execute purge batches`.

### Task 5: Recovery

**Files:** same crate.

- [ ] **Step 1: Failing test:** direct-SQL fabricated crash state (execution `started`, review `claimed`, child rows `pending`) → reopen → execution + pending child rows become `uncertain`; review stays claimed; a fresh review for the same receipts CAN be prepared (contract: later fresh review required for new actions); replay of the old review refused.
- [ ] **Step 2:** RED (open currently ignores the crash state). **Step 3:** implement recovery scan on open (exact journal rows only). **Step 4:** PASS. **Step 5:** Commit `feat: purge execution recovery marks uncertainty`.

### Task 6: Gate, docs, review, PR

- [ ] Full gate: `cargo test --workspace`, strict Clippy `-D warnings`, `cargo fmt --all --check`, `node scripts/check-version.mjs` (0.0.11), native `src-tauri` tests, migration probe tests.
- [ ] Update `docs/M1C-VALIDATION.md` (batch B section + honest limits: ledger/approval consumption, native confirmation, uncertainty cross-consultation, subprocess tests, accounting = Batch C/D; purge still unreachable from the UI) and both contract status lines; append the ledger. Commit `docs: record purge service validation`.
- [ ] Coordinator review subagent on the branch diff; close findings.
- [ ] Push; PR10 base `feat/m1c-purge-schema`; link; watch.

## Self-review

- Spec coverage: service-layer sections of `fixture-purge-v1` (API boundary, review freeze, review/claim/authority, control store, recovery, sanitized codes) → Tasks 1–5; ledger consumption + native confirmation + cross-store uncertainty + accounting + subprocess tests → Batch C/D, named in the validation doc.
- Step scan: every step names one artifact; no TBDs.
- Type consistency: `PurgeItemOutcome` (fixture-lifecycle) reused for per-item results via the batch outcome enum wrapping it with the child attempt id; `PurgeReceiptState` mirrors lifecycle snapshot statuses.
- Review Focus: each line pinned by Tasks 1/3/4/5 as listed.
- Proportion: plan shorter than the contract.
