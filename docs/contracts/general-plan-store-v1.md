# General plan store contract v1

## Scope and authority

`disk-steward-plan-store` persists the validated general editor, seeded with synthetic inventory and optionally extended with blocked native metadata entries from `disk-steward-plan-model`. Its projections and reviews remain `non_executable_editor` with `executionAvailable = false`. It exposes no scan, path, filesystem mutation, approval, or execution API. The trusted native owner supplies the database path.

`GeneralPlanStore::open(path, FixtureScale)` supports `Pilot1000` (1,000 findings / 25 groups) and `Stress10000` (10,000 / 25). The selected fixture is created only when the database is empty with no application ID, schema version, or non-internal SQLite objects. The profile is saved permanently; opening with another scale returns `fixture_profile_mismatch`. There is no reset or implicit migration.

## API and errors

The public methods are `get_plan()`, `edit(expectedRevision, PlanEdit)`, trusted `append_item(expectedRevision, InventoryItem)`, `undo(expectedRevision)`, `redo(expectedRevision)`, `freeze_review(expectedRevision)`, and `get_saved_review(reviewId)`, and bounded `list_reviews()` summaries. `append_item` is for a trusted native caller only; it accepts a validated metadata DTO, not a path, and is intentionally not representable as a renderer `PlanEdit`. It validates all current reviews, compares the expected revision under `BEGIN IMMEDIATE`, delegates to `PlanEditor::add_items`, and atomically persists the new archive and digest. Invalid, duplicate-ID, stale, and over-cap appends fail without changing the authoritative document. Added items use the ordinary model revision and undo/redo history. Every operation reloads the authoritative archive and imports it through plan-model validation. No in-memory editor cache is authoritative.

Stable `StoreError::code()` values are `stale_revision`, `fixture_profile_mismatch`, `unsupported_schema`, `corrupt_data`, `review_not_found`, `review_limit_reached`, `invalid_plan`, `invalid_edit`, and `storage_error`. A native wrapper can forward the code as a structured error discriminator.

## SQLite schema

Schema v1 uses application ID `0x44535053` and `user_version = 1`. Its complete user object set is exactly three `STRICT` tables: `store_metadata`, `current_document`, and `frozen_reviews`. Any other user table, index, view, or trigger is rejected. Tables are checked against the fixed v1 DDL on every read and mutation, inside that operation's SQLite transaction, so objects added while the app is open block subsequent operations before persisted plan data can change. SQLite internal `sqlite_*` objects are ignored. A database with a different application ID/version, unknown object, or invalid shape is preserved and refused; this crate has no migration/reset behavior.

The current document is the model's versioned archive bytes, bound to the row's revision ID and revision number and guarded by SHA-256. Appending inventory uses the existing archive `State.items` shape, so it adds no persisted field or schema migration. The archive remains size-limited in SQL before SQLite returns the BLOB, then its digest and all current/history states are checked by `PlanEditor::import_document_json`. The configured scale and fixed group count are checked against the imported snapshot; the scale is a fixture profile, not a hard limit on later trusted additions (the model's `MAX_ITEMS` limit still applies).

SQLite is opened with `NOFOLLOW`, and the trusted parent is canonicalized while the final leaf remains unresolved for SQLite's symlink check. Busy timeout is five seconds, journal mode is WAL, and synchronous mode is FULL. All schema and data writes occur in SQLite transactions.

## Frozen reviews

Reviews are immutable rows with unique review IDs and one row per revision. The stored row includes serialized `FrozenReview`, its SHA-256, and the model's deterministic `content_hash`. Reads bound the SQL byte length using the model's 32 MiB ceiling before loading JSON, verify the row hash and `FrozenReview::verify_content_hash()`, and compare the current revision's review content against a fresh projection. Only `reviewId` is ignored when comparing generated and saved review content. Historical reviews remain independently hash-verified across later edits and reopen.

At most 16 reviews are retained. When full, freezing a new revision returns `review_limit_reached`; no review is automatically removed. Every historical review must have the same `planId` as the current plan and a revision number no greater than the current monotonic revision. A review from another otherwise valid plan is corrupt even when its own content hash and row hash are valid. The only review type accepted is the domain model's non-executable editor review. It cannot authorize M1A fixture actions.
