# Fixture purge primitive (Batch A) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (native, established by prior sessions; ledger = `.autonomous/disk-steward/progress.md`) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `FixtureLifecycle::purge_receipt` — the typed, receipt-addressed permanent-removal primitive for staged disposable fixtures — with honest uncertain/terminal states, in `crates/fixture-lifecycle` only.

**Architecture:** Extends the existing stage/restore machinery: same per-call validation, intent-then-outcome attempt journaling, no-follow directory handles, and identity/digest verification. `purge_receipt` verifies the staged leaf against its registered identity (regular file, dev/ino, one link, exact bytes, content digest) via the opened holding-directory handle, journals a `purge` intent, unlinks directory-relative through that same handle, syncs the holding directory, and records the terminal outcome. Any failure that cannot establish the result becomes `Uncertain`; only a fully verified, synced, durably-recorded unlink is `PermanentlyRemoved`.

**Tech Stack:** Rust 2021, rusqlite (IMMEDIATE transactions + WAL checkpoint), rustix (no-follow opens, `unlinkat`, directory fsync), serde snake_case DTOs.

**Spec:** `docs/contracts/fixture-purge-v1.md` (frozen proposal). This plan implements only the `FixtureLifecycle::purge_receipt` layer plus the vocabulary/validator/recovery changes it requires. `FixturePurgeService`, the `fixture-purge-control-v1.sqlite3` control store, review/confirmation/claims, ledger consumption, native confirmation and accounting are later batches — none are touched here.

## Global Constraints

- No caller paths, no `approved: bool`, no renderer authority; the only address is `(session, receipt_id)` via registered slots. (`fixture-purge-v1`: API boundary)
- Unlink is directory-relative through the already-opened holding-directory handle (`rustix::fs::unlinkat`), never a caller path, never recursive, never a directory. (`fixture-purge-v1`: filesystem section)
- Purge targets only receipts whose fixture row is exactly `status='staged'`; everything else is refused without mutation.
- The schema v2 "reserved" vocabulary becomes active for the purge operation only: attempts `operation='purge'` with outcomes `intent`/`permanently_removed`/`uncertain`, and fixtures `status='permanently_removed'`. All other reserved values stay rejected. (CHECK constraints already permit these.)
- Append-only history: `record_intent` inserts the intent row; `record_outcome` updates that same attempt row's outcome. No attempt rows are deleted or rewritten.
- Errors are the existing `LifecycleError` variants (crate convention). The contract's `PurgeError` placeholder belongs to the batch/service layer (Batch B), which will wrap these.
- Tests use only fresh disposable fixtures in tempdirs; never real user data or installed AppData. Unix-only, matching CI (`all(test, any(linux, macos))` gates already in place).
- Versions stay 0.0.10; no release. PR targets `feat/m1c-purge-schema`. No snapshot, Trash, or free-space claims: `logical_bytes` is the registered fixture length only.

## Review Focus

- **Same-user TOCTOU between final verify and the directory-relative unlink:** a reasonable person expects no claimed atomic-inode-unlink guarantee. The code and validation doc must state the residual race honestly and report uncertainty when the result cannot be established; no test can pin the race itself in-process — the doc statement is the deliverable (Task 3).
- **Any unlink failure is unestablishable, not a no-op:** expectation: `Uncertain`, never silent success, never blind retry. Pinned by the read-only-directory test (Task 3).
- **A crash after intent but before outcome must never claim success nor auto-retry:** expectation: snapshot `Uncertain`, restore and purge refused until inspection. Pinned by the injected-intent reopen test (Task 4).
- **A purged receipt is permanently unrestorable:** expectation: restore refused at any later time; snapshot never re-verifies the (now absent) file into `Changed`. Pinned by Task 5's terminal-refusal tests.
- **Purging one receipt leaves every other fixture byte-identical and unstaged items untouched:** expectation: exact-target serial behavior. Pinned by Task 5's five-fixture batch test.

---

### Task 1: Active purge vocabulary in validators, snapshot, and `FixtureStatus`

**Files:**
- Modify: `crates/fixture-lifecycle/src/lib.rs` (fixture status validator ~line 774; attempts outcome validator ~line 840; snapshot mapping ~line 437; `FixtureStatus` enum ~line 37)

**Interfaces:**
- Produces: `FixtureStatus::PermanentlyRemoved` (serde `"permanently_removed"`); validators accept stored `status='permanently_removed'` (with `receipt_id` present) and attempts rows `operation='purge'` with `outcome ∈ {intent, permanently_removed, uncertain}`. Later tasks and Batch B rely on these.

- [ ] **Step 1: Write failing tests** (one test fn, three assertions):
  - stored fixture row `status='permanently_removed'` (via direct SQL on a staged session) → `snapshot()` returns `FixtureStatus::PermanentlyRemoved` and does NOT report `Changed` (file is expected to be gone); currently fails (validator `InvalidSession`).
  - direct-SQL attempts row `(operation='purge', outcome='permanently_removed')` validates; today rejected.
  - attempts row `(operation='purge', outcome='staged')` still rejected `InvalidSession` (regression pin).
- [ ] **Step 2: Run** `cargo test -p disk-steward-fixture-lifecycle` → new test FAILS.
- [ ] **Step 3: Implement:** add `"permanently_removed"` arm to the fixture status validator (requires `receipt_id.is_some()`); add `"purge"` arm to the attempts outcome validator (`intent`/`permanently_removed`/`uncertain`); add `PermanentlyRemoved` to `FixtureStatus`; snapshot: for stored `state == "permanently_removed"`, skip file verification and map to `FixtureStatus::PermanentlyRemoved` (keep `receipt_id`).
- [ ] **Step 4: Run full lifecycle suite** → PASS (20 prior + 1 new).
- [ ] **Step 5: Commit** `feat: activate purge vocabulary in runtime validator and snapshot`.

### Task 2: `purge_receipt` happy path and pre-unlink refusals

**Files:**
- Modify: `crates/fixture-lifecycle/src/lib.rs` (`record_intent` allowed_state match; `record_outcome` fixture-status update; new `pub fn purge_receipt` next to `restore`; new `pub enum PurgeItemOutcome`)

**Interfaces:**
- Produces:
  - `pub enum PurgeItemOutcome { NotAttempted { receipt_id: ReceiptId }, PermanentlyRemoved { receipt_id: ReceiptId, logical_bytes: u64 }, Uncertain { receipt_id: ReceiptId } }` (serde snake_case; `NotAttempted` is constructed by the Batch B batch layer only)
  - `pub fn purge_receipt(&mut self, receipt: ReceiptId, attempt: AttemptId) -> Result<PurgeItemOutcome, LifecycleError>`
  - `record_intent` gains `"purge" => current.status == "staged" && current.receipt_id == Some(receipt-wire)` (no already-issued-receipt refusal — the receipt was legitimately issued by stage); `record_outcome` gains: outcome `"permanently_removed"` → `UPDATE fixtures SET status='permanently_removed'` (receipt unchanged).

- [ ] **Step 1: Write failing tests:**
  - happy path: create → stage → `purge_receipt` → `Ok(PermanentlyRemoved { receipt_id, logical_bytes: registered })`; holding slot gone (open_regular fails); attempts row outcome `permanently_removed`; snapshot `PermanentlyRemoved`; other four fixtures byte-identical (`Available`, sentinel contents unchanged).
  - `purge_receipt` on an `available` fixture's receipt → `Err(WrongState)`, no attempt row, no mutation.
  - unknown receipt → `Err(UnknownReceipt)`.
  - changed content (tamper holding file) → `Err(Changed)`, no attempt row, file still present, snapshot still `Staged`-with-pending? no — exactly unchanged `Staged`.
- [ ] **Step 2: Run** → FAIL (`purge_receipt` not defined).
- [ ] **Step 3: Implement** `purge_receipt`: `validate_schema`+`validate_registry`; `lookup_receipt`; pending-intent → `Uncertain`; `status=="staged"` else `WrongState`; `verify_fixture(holding, receipt_slot)` else `Changed`; `record_intent(..., "purge", Some(receipt))`; `unlinkat(&self.holding_dir, slot, AtFlags::empty())`; `sync_dir(&self.holding_dir)`; `record_outcome(attempt, action, "permanently_removed", Some(receipt))`; return `Ok(PermanentlyRemoved { receipt_id, logical_bytes })`. Any unlink/sync/outcome failure → `self.uncertain_error(attempt, action)` (mirrors stage/restore).
- [ ] **Step 4: Run full suite** → PASS.
- [ ] **Step 5: Commit** `feat: add receipt-addressed purge primitive to fixture lifecycle`.

### Task 3: Unlink/sync failure boundary — uncertainty, not guessing

**Files:**
- Modify: `crates/fixture-lifecycle/src/lib.rs` (tests only; production flow already routes unlink/sync failures through `uncertain_error`)

**Interfaces:** consumes Task 2's `purge_receipt`.

- [ ] **Step 1: Write failing-context test:** stage → make holding directory non-writable (`set_permissions(0o500)`) → `purge_receipt` → `Err(Uncertain)`; attempts row outcome `uncertain`; fixture status `uncertain`; the file itself still present (unlink refused). Restore permissions; assert restore is now refused (`Uncertain` — pending? no: status uncertain → `WrongState`... use the actual observed variant, pin it). (On macOS `unlinkat` into a 0o500 dir owned by the user fails with EACCES; if the runner proves otherwise, stop and investigate — do not weaken the assertion.)
- [ ] **Step 2: Run** → assert matches reality; the test pins `Err(Uncertain)` + no success claim.
- [ ] **Step 3: Implement** only whatever the test exposes (expected: nothing — flow already correct). If the test passes immediately, it is still a valid regression pin; note that in the commit message.
- [ ] **Step 4: Full suite** → PASS.
- [ ] **Step 5: Commit** `test: pin unlink-failure uncertainty for purge`.
- [ ] **Step 6:** Document the residual same-user TOCTOU in the `purge_receipt` doc comment (verify→unlink is name-bound, not inode-atomic; portability lacks an atomic conditional unlink; directory privacy/handles are the mitigation; uncertainty is reported when unestablishable) — fold into this commit.

### Task 4: Crash after intent — reopen keeps `Uncertain`, never retries

**Files:**
- Modify: `crates/fixture-lifecycle/src/lib.rs` (tests only)

**Interfaces:** consumes Task 2's `record_intent` (private, in-module test access).

- [ ] **Step 1: Write test:** stage → `record_intent(attempt, &fixture, "purge", Some(receipt))` (simulated crash before unlink) → drop lifecycle → `reopen` → snapshot `Uncertain` for that fixture (and file still present in holding); `restore` → `Err(Uncertain)`; `purge_receipt` → `Err(Uncertain)`; the intent row is untouched (no auto-resolution, no retry).
- [ ] **Step 2: Run** → pin actual behavior (expected PASS already; regression pin).
- [ ] **Step 3: Full suite** → PASS. **Commit** `test: purge intent crash leaves uncertain without retry`.

### Task 5: Terminal refusal and exact-target batch behavior

**Files:**
- Modify: `crates/fixture-lifecycle/src/lib.rs` (tests only)

**Interfaces:** consumes Tasks 2–4.

- [ ] **Step 1: Write tests:**
  - after `PermanentlyRemoved`: `restore` → `Err(WrongState)` (any later time, incl. after reopen); `purge_receipt` again → `Err(WrongState)`; snapshot stays `PermanentlyRemoved` with the same `receipt_id`.
  - serial five-fixture batch: stage 3 → purge all 3 → three `PermanentlyRemoved`, two untouched `Available`, source/holding sentinels byte-identical, attempt history append-only (rowids ordered, no rewrites — compare full `dump_all` before/after where applicable).
- [ ] **Step 2: Run** → PASS (or fix surfaced gaps). **Commit** `test: purge terminal refusal and exact-target serial batch`.
- [ ] **Step 3: Full verification gate:** `cargo test --workspace` (134), `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, `node scripts/check-version.mjs` (0.0.10), native `cargo test` in `src-tauri` (26).
- [ ] **Step 4:** Update `docs/M1C-VALIDATION.md` with a Batch A section (evidence + honest limits: TOCTOU residual, subprocess-boundary tests and coordinator/native flows deferred to Batches B/C, controlled-volume accounting deferred). Append the ledger entry. **Commit** `docs: record purge primitive validation`.

## Self-review

- Spec coverage: lifecycle-layer items of `fixture-purge-v1` (typed API boundary, filesystem transitions, outcome/receipt states, intent journal, uncertainty, TOCTOU honesty, logical-bytes) → Tasks 1–5 + docs. Coordinator/service, control DB, reviews/claims/ledger, native confirmation, accounting → explicitly Batch B/C/D, not silently dropped.
- Step scan: each step names one artifact; no `TBD`s; bodies only where the contract fixes them (flow order is contract-mandated).
- Type consistency: `PurgeItemOutcome` variant names/fields match the contract sketch exactly (`PermanentlyRemoved { receipt_id, logical_bytes }`); `logical_bytes` = registered fixture bytes.
- Review Focus: each line pinned by Task 3 (read-only dir), Task 4 (crash), Task 5 (terminal + serial batch); TOCTOU doc is Task 3 Step 6.
- Proportion: plan shorter than the contract; no code transcripts.
