# Disposable file execution contract v1

M1A next slice. This service can operate only on the five files created by FixtureLifecycle. No existing user files, arbitrary paths, model calls, permanent removal or general recursive operations are supported. Existing kind=non_executable_example review IDs never authorize anything. Every file operation requires a newly prepared disposable-file review and backend-owned native confirmation.

Rust service (crates/demo-execution, package disk-steward-demo-execution):
- DemoExecutionService::open(app_owned_root:&Path)
- status() -> DemoStatus
- start_demo() -> DemoStatus (idempotent one active session; persist creation intent with SessionId BEFORE FixtureLifecycle::create_for_session; failure/restart never regenerates or deletes an uncertain session)
- prepare_stage(expected_revision:RevisionId) -> PreparedDemoReview
- prepare_restore(expected_revision:RevisionId,receipt:ReceiptId) -> PreparedDemoReview
- confirm_and_execute(review_id:ReviewId, confirmer:FnOnce(&PreparedDemoReview)->bool) -> DemoExecutionResult

The native coordinator supplies the confirmer; no IPC accepts a boolean approval, paths or a manifest. The callback is tested with synthetic confirmations but in the app must open a parented OS-native dialog containing the exact backend targets, operation, sizes, destination and recovery consequences. Revalidate current plan revision, prepared hash, expiry and fresh session identities/status both before and AFTER callback, before any filesystem mutation. Cancel performs no file mutation.

Use ReviewPlanStore at root/review-example-v1.sqlite3 for authoritative decisions. This metadata remains non-executable; bind it into a distinct private versioned disposable-file manifest with SessionId, ScopeId, exact selected ActionIds, every fixture identity/content/status/receipt, operation and adapter version. Fixed serializable structs provide canonical hashing. Store prepared reviews durably with a short expiry (5minutes default); reject wall-clock reversal, expiry, corruption and reuse. App control database stores one session binding and execution/child-attempt records; ApprovalLedger remains separate. Exact known schema/objects, no-follow leaf opening with canonical trusted parent, no URI paths, bounded JSON and transactions are required; never reset unknown data.

On accepted confirmation, atomically claim the unused review and persist batch ExecutionId/ApprovalId and child AttemptIds BEFORE file operations. Reconstruct a fresh typed Core PlanSnapshot/Approval, validate it, and insert/consume its exact digest once through ApprovalLedger, then record its execution intent. No filesystem calls happen unless all these steps succeed. Crash gaps fail closed. Child AttemptIds are unique per item; batch ExecutionId is never reused as an item attempt.

Execute only the frozen selected items, using component stage/restore by ID. Known partial success is reported accurately; uncertain outcomes remain uncertain and retain data. Do not automatically retry, roll back, overwrite, regenerate or delete. Persist outcomes; after interruption, read journals/status and show incomplete/uncertain results without resuming. Native commands will be serialized per service; SQLite claims must also prevent duplicate work across instances.

The service holds an exclusive operating-system file lease for its entire lifetime, acquired before opening or recovering control records. A second live instance fails busy and must not reconcile the first instance's active work. Only a subsequent owner may mark interrupted work uncertain. Releasing ownership never authorizes a retry. Failure to durably finish a claimed execution leaves it uncertain; an error after a file move must not claim that nothing changed.

Both stage and restore use a same-volume no-overwrite rename. Restore requires the original demo name to be unoccupied. Neither operation reports recovered space; a future separately approved permanent-removal operation is outside this contract.

Public DTOs use camelCase and core typed IDs. Suggested fields:
- DemoStatus: schemaVersion1, kind=disposable_file_demo, state=not_started|ready|needs_inspection, sessionId(optional), fixtures[], lastExecution(optional).
- Fixture item: actionId, receiptId(optional), status, bytes, sha256, identitySha256. No paths.
- PreparedDemoReview: schemaVersion1, kind=disposable_file_review, reviewId, planId, revisionId, sessionId, operation=stage|restore, items[{actionId,label,bytes}], selectedBytes, manifestSha256, expiresAtMs. Labels come only from the validated plan.
- DemoExecutionResult: executionId, operation, state=finished|partial|uncertain, items[{actionId,receiptId(optional),status}], stagedBytes/restoredBytes as appropriate, recoveredBytes always0 (staging/restoring frees no space).

Explicit service errors have sanitized stable codes; native bridge never forwards raw paths/SQL/errors. No full Approval value is serialized. IDs and review/receipt references are not bearer permissions.

Tests: complete stage/restore/reopen, cancel unchanged, stale edit and content change DURING confirmation refused, review replay/expiry refused, crash/started job recovery without retry, session creation intent avoids orphan retry growth, source/restore collisions preserve sentinels, unknown schema/extra objects preserved, partial outcomes retain receipts and data. All fixtures remain disposable isolated test data. No production adapter availability follows from these tests.
