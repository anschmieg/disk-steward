# General plan editor contract v1

## Scope

This contract describes a pure domain editor over a bounded inventory snapshot. It is separate from the M1A five-file demonstration and carries no filesystem, execution, approval, persistence, or service authority. All emitted projections and frozen reviews set `kind = non_executable_editor` and `execution_available = false`.

## Input and identity

The editor accepts a `PlanId`, revision identity, immutable inventory items keyed by `InventoryItemId`, evidence references keyed by `EvidenceId`, explicit groups keyed by `GroupId`, and duplicate sets keyed by `DuplicateGroupId`. Item records contain parent identity, display label, logical byte count, conservative recovery lower/upper bounds, storage-sharing identity where known, hard blockers, evidence references, and a suggestion. Suggestions are immutable input facts; user choices are stored independently.

Inventory and group validation is atomic. IDs are unique and references resolve; labels and evidence are bounded; the rooted item graph is acyclic, at most 64 levels deep, and has at most 10,000 items; total group/duplicate memberships, history, and serialized state are bounded; byte bounds are ordered. A duplicate set is valid only for at least two files with the same known logical size. A set without a survivor is unresolved: every member receives a `duplicate_survivor_required` blocker and cannot be included until the user selects a survivor. Malformed input is rejected with a typed validation error. No renderer path is accepted.

## User choices and projections

User choice is one of `Keep`, `Include`, or `Investigate`. Plan membership is an independent boolean, so removing an item from the plan does not remove its inventory record, while re-adding preserves its decision. Suggestions never silently replace user choices during refresh. An explicit folder `Keep` or `Investigate` suppresses automatic descendant suggestions; an explicit child `Include` can override that intent when the child is eligible and not excluded. Group Include preserves explicit item Keeps and reports `kept_by_user`. Refresh reconciles by stable item identity and preserves explicit choices, membership, descendant exclusions, and duplicate survivors where the referenced entities remain valid.

An explicit descendant exclusion names inventory identities. Excluded descendants are kept out of the parent selection projection; an unknown descendant or one outside the selected subtree is rejected. Duplicate cleanup requires an explicit survivor; survivor changes recompute candidate membership and estimates, and cannot select the survivor for removal. Group edits apply only to eligible members and preserve blockers; blocked Include requests remain unselected and return a per-member reason.

Each item/group projection exposes its suggestion, user choice, membership, effective selection, blocker reasons, logical selected bytes, recovery estimate lower/upper bounds, and a clear non-executable status. Logical bytes and recovery bounds are different measures. Actual recovered bytes are absent. Parent/child overlap is counted once through selected leaf accounting; hard-linked/shared storage identities contribute at most once to conservative recovery bounds. Uncertain overlap or shared allocation lowers the bound or makes it unknown rather than inflating it.

## Editing, revisions, and reviews

Every meaningful edit creates a fresh `RevisionId`, including undo and redo. A semantic no-op preserves the current revision. Edit requests include an expected revision; stale requests fail without mutation. Undo and redo retain bounded snapshots, and a new edit after undo discards redo history. History depth and total serialized input/state are bounded.

A frozen review is an immutable snapshot identified by `ReviewId`, tied to one revision, and summarized by a deterministic hash over a fixed v1 manifest with ordered collections (never hash-map iteration order). The manifest covers schema/kind/execution flag, plan/revision identity and number, item/group/duplicate projections, and totals; it excludes transient undo/redo availability and the generated review ID. `FrozenReview::verify_content_hash()` also rejects an unknown schema/version or any executable flag. Subsequent edits cannot mutate an existing review. Reviews remain `non_executable_editor`; they do not authorize M1A operations or any cleanup.

## Frozen frontend DTO

All external JSON uses camelCase field names, rejects unknown fields, and serializes choice as the lowercase strings `keep`, `include`, `investigate`. Edit discriminants use camelCase strings. These names and meanings are frozen for the first frontend integration.

`EditorSnapshot` contains `schemaVersion`, `kind` (`non_executable_editor`), `executionAvailable` (always false), `planId`, `revisionId`, `revisionNumber`, `items`, `groups`, `duplicates`, `totals`, `canUndo`, and `canRedo`.

Each item row contains `itemId`, nullable `parentId`, `label`, `kind` (`file`/`folder`), nullable `logicalBytes`, `suggestion` (`choice`, `rationale`), nullable `userChoice`, `inPlan`, `effectiveSelected`, `blockers` (`code`, `reason`), `evidence` (`id`, `summary`), `excludedDescendantIds`, `groupIds`, nullable `recoveryLowerBytes`, and nullable `recoveryUpperBytes`. `effectiveSelected` is the editor's authoritative selection projection; the UI does not infer selection from ancestry. Numeric logical totals are known-size subtotals; rows with unknown logical size remain null and must not be presented as zero-size items.

Each group row contains `id`, `label`, `memberIds`, `selectedCount`, `selectedLogicalBytes`, and `blockedCount`. Each duplicate row contains `id`, `memberIds`, and nullable `survivorId`. Totals contain `selectedCount`, `logicalSelectedBytes`, `inPlanLogicalBytes`, `recoveryLowerBytes`, and nullable `recoveryUpperBytes`. `storageIdentity` is a complete known shared-object identity, not a path. Selected logical bytes count unique leaves and deduplicate a known shared identity. Recovery lower bounds are zero for multiply referenced shared identities and upper bounds count the shared identity once; if a known alias remains unselected for removal, its recovery upper bound is zero. If sharing/allocation is uncertain, provide unknown bounds (`null` lower and upper) instead of inventing recoverable space. These values are distinct from actual recovered bytes, which are not part of this contract.

`PlanEdit` is an externally tagged object with a `kind` discriminator and camelCase fields: `setChoice {itemId, choice}`, `setMembership {itemId, inPlan}`, `setGroupChoice {groupId, choice}`, `excludeDescendants {parentId, itemIds}`, `setDuplicateSurvivor {duplicateSetId, survivorId}`. `EditOutcome` contains `snapshot`, `blocked` (`itemId`, `reason`), and `changed`.

The Rust crate exposes `PlanEditor::new(input) -> Result<PlanEditor, ValidationError>`, `snapshot() -> EditorSnapshot`, `edit(expectedRevision, PlanEdit) -> Result<EditOutcome, EditError>`, `undo(expectedRevision)`, `redo(expectedRevision)`, `refresh(expectedRevision, InventoryRefresh)`, and `freeze_review(expectedRevision) -> FrozenReview`. The editor is a value-only domain component and performs no I/O. No changes to these DTO names should be made without coordinating with the frontend owner.

Persistence batches may store `export_document_json()` bytes and restore them with `PlanEditor::import_document_json(bytes)`. The archive is a private versioned deny-unknown-fields schema containing only authoritative inventory/user state, revision identity/number, and bounded undo/redo states; projections and frozen reviews are derived/stored separately. Import validates the current and every historical state before restoring it. State JSON is capped at 24 MiB, combined history at 8 MiB/64 entries per direction, and the complete archive at 32 MiB. Oversized input is rejected before deserialization.

`synthetic_fixture(1_000, 25)` produces exactly 1,000 inventory records in 25 folder groups with synthetic evidence, valuable notes, generated entries, blockers, unknown sizes, known shared storage, and three same-size file duplicate sets (including one unresolved survivor). `synthetic_fixture(10_000, 25)` is a separate bounded stress input. These fixtures use generated IDs and labels only; their automated checks do not satisfy the human review pilot.

Group selected counts use the same union of selected leaf items as whole-plan counts, including descendants when a group names their ancestor. Parent folders are not counted again. Unknown logical sizes contribute no invented bytes; the interface labels known portions and reports unknown leaf counts from the inventory.
