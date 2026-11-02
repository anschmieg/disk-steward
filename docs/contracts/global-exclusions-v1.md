# Global directory exclusion registry v1

## Scope and authority

`disk-steward-exclusion-store` persists exact directory location bindings chosen through a native file picker and separately confirmed in native UI. It is a registry only: it does not enumerate the bound directory, open its contents, grant filesystem access, authorize cleanup, or assert that a rule matched a scan. M1B can display saved rules as intended for future scans. M2 must consume them before traversal using an independently selected and authorized scope.

The registry is distinct from the synthetic general-plan store. The current synthetic items and M1B metadata-only file additions have no persisted source binding, so registry edits do not alter their decisions, revisions, reviews, or coverage. The UI must state that saved exclusions are not applied to the example plan. M1B completion and scan coverage are not established by registry persistence.

## Native caller and DTO boundary

The trusted native owner supplies a `NativeBinding` produced from an exact selected directory. Version 1 stores a bounded lossless absolute Unix locator byte sequence (raw path bytes, beginning with `/`, with no encoding prefix), a nonempty volume identity, a nonempty directory resource identity, and a bounded sanitized basename. The store treats locator and identity bytes as opaque values and performs no filesystem observation beyond the metadata checks needed to protect its own database. The type has no `Debug` or Serde implementation. Its read-only byte accessors are for native confirmation and identity comparison only. The store rejects empty, oversized, control/bidi-containing, slash-containing, dot, dot-dot, or edge-whitespace basenames.

The public API is:

- `Registry::open(path)`, `snapshot()`, and native-only `get_binding(id)`;
- `add_confirmed(expectedRevision, NativeBinding)` and `remove_confirmed(expectedRevision, id)`.

The `*_confirmed` method names document a caller precondition; the storage crate cannot prove that a native confirmation occurred. Renderer IPC must never call these methods directly or submit a path, binding, identity, or approval boolean. Native handlers must perform selection, present a separate native confirmation, revalidate the selected identity after confirmation, and only then call the mutation method. Cancellation leaves the registry unchanged.

`RegistrySnapshot` serializes as `{schemaVersion, revisionId, entries}`. Each public row contains only `{id, basename, status}`. `id` is an opaque lookup reference, not a permission token. This store returns `saved_unverified` for every row. `stale` and `unavailable` are transient statuses a native scan adapter may report after a fresh observation; no active or matched status is stored in SQLite.

## Identity and matching

An exclusion names one exact selected directory location. It is not a descendant list supplied by the renderer, pattern, category rule, filesystem permission grant, or cleanup action. The native adapter must reject non-directories, symbolic-link final components, and selections for which volume or directory identity is unavailable. The binding's encoding version is fixed at 1; bytes must round-trip losslessly, including non-UTF-8 native path bytes. The adapter must not use a display string conversion as a durable identity.

The registry rejects duplicate locator bindings whose volume or directory identity differs. An exact locator and identity duplicate is a no-op and retains the previously saved display basename. Replacing a directory at the same path does not silently update the record; the existing binding must be explicitly removed and a fresh binding separately confirmed. Moving a directory or changing its volume leaves the old exact-location rule unmatched. No rule is considered active from a stored path alone, a stored object ID alone, or a prior access grant.

On a future scan, the scan adapter independently obtains its selected-scope permission, opens the traversed directory without following the final symlink, and freshly checks the exact locator, volume identity, and directory resource identity before skipping traversal beneath that directory. A missing/ejected volume or denied permission is unavailable; a path resolving to a different object is stale; an unavailable identity remains unverified. None of these cases may be reported as inspected or clean. A registry rule never grants scan access.

## Persistence and privacy

The registry uses its own app-owned SQLite file and schema, separate from `editor-pilot-v1.sqlite3` and `editor-stress-v1.sqlite3`. Schema v1 uses application ID `0x44534558`, `user_version = 1`, and exactly two `STRICT` tables: `directory_exclusions` and `registry_metadata`. Locator bytes have a unique constraint. Any unknown user table, index, view, trigger, schema version, or shape is rejected without resetting or rewriting the database. There is no implicit migration.

The native owner creates and owns a private parent directory. On Unix, the store requires parent permissions with no group/world bits, creates a new database file with mode `0600`, and refuses an existing database with broader permissions without changing its mode. On platforms where the store cannot verify the required privacy conditions, opening fails closed. SQLite uses `NOFOLLOW`, WAL, `synchronous=FULL`, and immediate write transactions. Limits are 256 rows, 16 KiB per locator, 1 KiB per identity value, 512 UTF-8 bytes per basename, and 128 bytes per typed identifier; the combined locator/identity bytes are also bounded. SQL length filters cap IDs, hashes, basenames, locators, identities, and registry revision before loading their values. Schema object names and definitions are bounded before loading. Every stored binding has a SHA-256 digest over a fixed ordered v1 manifest including its ID and fields. Oversized rows are rejected by SQL length filters before blobs are returned to Rust; row-count mismatches, digest errors, and malformed values are corruption errors. These unkeyed digests detect inconsistent data; they are not authentication against an actor who can rewrite the private database.

Raw locator and identity bytes stay in the private registry and in native memory. They must not enter renderer DTOs, logs, diagnostics, error text, or review JSON. The registry's public errors are fixed sanitized codes and do not retain the underlying SQLite error string. This is local filesystem privacy, not encryption at rest.

## Revisions and API behavior

Every semantic add/remove uses `BEGIN IMMEDIATE`, compares an expected typed `RevisionId` while holding the database write lock, validates the complete known schema and all rows, commits the binding change, and assigns a fresh `RevisionId`. Exact duplicate add preserves the revision. A stale revision, binding conflict, invalid binding, row limit, unknown ID, unsupported schema, corrupt data, or privacy failure performs no mutation. Removal is by typed ID and is serialized against concurrent changes. The registry does not implement undo or change plan review hashes; native UI confirmation is separate from database revision control.

Stable error codes are `stale_revision`, `binding_conflict`, `unsupported_schema`, `corrupt_data`, `entry_not_found`, `privacy_unavailable`, `registry_limit`, `invalid_binding`, and `storage_error`. Error details never include paths, identities, SQL, or underlying OS messages.

## Validation boundary

Automated storage fixtures can verify schema refusal, data integrity, privacy mode checks, non-UTF-8 byte roundtrips, concurrency, and stale/replaced identity comparisons supplied by a fake adapter. They cannot establish packaged macOS picker, security-scope, permission denial/revocation, volume-remount behavior, or identity-provider stability. Those remain native/package and M0 gates. M2 must add end-to-end checks proving exclusions are matched before traversal and coverage is reported accurately.
