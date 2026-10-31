# Non-executable example-plan contract v1

This contract is for M1A's first review-only slice. All records are synthetic metadata stored locally. They do not refer to filesystem targets, and no review is an approval. Version0.0.4; first-party crate name disk-steward-review-plan.

Rust service: ReviewPlanStore::open(path), get_plan(), edit(expected_revision:RevisionId, action:ActionId, decision:Decision), freeze_review(expected_revision:RevisionId). Return typed Result values with explicit stale/unknown/empty/schema/storage failures. Open seeds exactly one five-action plan if the owned new database is empty. Reopen preserves IDs/decisions/revisions/reviews. No renderer paths, SQL, file creation, model calls or execution APIs.

JSON uses camelCase fields and lowercase decision values keep/include. All IDs serialize through disk-steward-core typed ID v1 encodings.

PlanView:
- schemaVersion:1; kind:"non_executable_example"; planId:PlanId; revisionId:RevisionId; revisionNumber:u64.
- actions:ActionView[] in stable display order.
- totalBytes:u64; selectedBytes:u64; selectedCount:u32; executionAvailable:false.

ActionView: id:ActionId; label:string; category:"build"|"cache"; bytes:u64; decision:"keep"|"include"; evidence:EvidenceView[].
EvidenceView: id:EvidenceId; summary:string. Evidence explicitly describes a disposable synthetic example, never a real detector certification.

FrozenReview: schemaVersion:1; kind:"non_executable_example"; reviewId:ReviewId; planId:PlanId; revisionId:RevisionId; revisionNumber:u64; manifestSha256:64lowercasehex; actions:ActionView[]; totalBytes:u64; selectedBytes:u64; selectedCount:u32; executionAvailable:false. Hash a fixed versioned serializable struct containing the exact plan/revision/actions/decisions/evidence/totals/kind and adapter contract version. Exclude reviewId/hash themselves. Retain snapshots after edits; old revisions never match current execution/review preconditions. Repeated freeze of unchanged revision can return its existing immutable review.

Five fixtures (all initially keep): Compiler output/build3145728bytes; Test cache/cache1048576bytes; Preview cache/cache524288bytes; Package index/cache262144bytes; Incremental output/build131072bytes. Total5111808bytes (4.875MiB). IDs/evidenceIDs are generated once and persisted. No claim of real tool provenance or actual filesystem savings.

Tauri commands owned by coordinator:
- get_example_plan() -> PlanView
- edit_example_plan(request:{expectedRevisionId,actionId,decision}) -> PlanView
- freeze_example_review(request:{expectedRevisionId}) -> FrozenReview

Native handlers resolve database storage internally and expose only these input types. Frontend has no execution request in this slice. Browser adapter may display the same non-executable fixture but must label unsaved preview and never imply desktop persistence.

Command errors are sanitized JSON objects {code:string,message:string}. Known codes: stale_revision, unknown_action, nothing_selected, unsupported_schema, storage_unavailable. Never return paths, raw SQL or credential details in these errors.
