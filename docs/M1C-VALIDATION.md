# M1C validation — runtime fixture schema v2 adoption

Version 0.0.9, branch `feat/m1c-lifecycle-schema`, based on `ed49cd8` (PR7, `feat/m1c-purge-schema`). This adopts the reviewed v1→v2 fixture-lifecycle schema migration into the runtime crate. It introduces **no purge API, no unlink or deletion capability, no new native command, no UI change, and no space-reclaimed claim**. Reserved future purge vocabulary (`operation='purge'`, `status='permanently_removed'`, `outcome='permanently_removed'`) exists only at the CHECK-constraint layer and remains rejected by the runtime allowlist validator.

## What changed

- `FixtureLifecycle::reopen` now migrates a legacy schema-1 fixture database in place: pre-transaction validation of exact v1 schema, manifest identity, row bounds and registry; then an `IMMEDIATE` transaction that rebuilds only the `fixtures`/`attempts` CHECK tables with explicit `rowid`, all columns and `ORDER BY rowid` preservation; `user_version=2` is committed last after full schema validation. Unknown/future schema versions and extra schema objects fail closed with no reset.
- New sessions are created directly at schema 2; stage/restore/snapshot re-run schema and registry validation per call.
- Coordinator guard: `DemoExecutionService::ready_session` refuses to lazily reopen fixtures when the owner lease is not held (`ensure_active`), because reopen may now perform a write migration. Contract `fixture-runtime-schema-v2` requires the lease for the complete reopen. Status refresh with the lifecycle handle still held stays usable after a poison, so the UI can show uncertain state.

## Evidence

- 133 workspace Rust tests pass (131 previous plus two new review-finding tests), including all 21 fixture-lifecycle and 15 demo-execution tests. Strict Clippy `-D warnings`, `cargo fmt --check` and `scripts/check-version.mjs` (0.0.9) pass; frontend `npm ci && npm run build` passed earlier on this branch with no frontend changes since.
- Migration tests dump and compare **every rowid and every column** of manifest, fixtures and attempts: populated session (rowid gaps +10000, two staged fixtures) and empty session, before vs after migration; after each injected fault (`AfterRename`/`AfterCreate`/`AfterCopy`/`BeforeVersion`) with exact `InvalidSession` identity, unchanged `user_version` and byte-identical table dumps; and after a successful migration following earlier faults. RED phases were proven by mutation (removing explicit rowids; committing at a fault point) and reverted.
- Reserved vocabulary: all three reserved tokens are asserted rejected by the runtime validator (`InvalidSession`) **and** preserved in the database untouched.
- Future-version refusal: `reopen` on `user_version` 0, 3, 9 and `i32::MAX` fails with `InvalidSession` and leaves `user_version` and every row byte-identical (verified by raw re-open).
- Native backend: 26/26 tests pass on 0.0.9. A debug bundle (`Disk Steward.app`, 40.17 MiB, plus `Disk Steward_0.0.9_aarch64.dmg`) was built and launched with a disposable `HOME`; the process started cleanly, created a fresh private app-data directory, and was terminated cleanly. No real user data or installed AppData was touched.
- Independent coordinator review of the full branch diff found **no critical findings**. Important findings were addressed (leaseless-reopen gate with regression test; reserved-vocabulary test extended to all three tokens). Minor findings addressed: exact fault-error identity, runtime future-version test, downgrade-baseline preservation assertion. Accepted without code change: fault-injection scaffolding compiled into production (private, only called with `MigrationFault::None`; no capability leak), and a theoretical concurrent-reopen race that fails closed under the single-lease-coordinator assumption.

## Open gates

- CI currently fails cross-platform `core` tests **pre-existing on the PR7 base head** (`ed49cd8`): Windows fails in `fixture_service()` with the by-design Unix-only `SessionUnavailable` lease error; ubuntu/macos were cancelled as dependents. This is not introduced by this branch and needs its own triage.
- Migration exercised through real app startup was not seeded (no v1 fixture session existed in the disposable home); reopen-migration is covered at crate level and the coordinator gate at unit level. A seeded-disposable-home startup pass remains for the M1C native confirmation gate.
- The separately reviewed purge authority (contract `fixture-purge-v1`) remains unimplemented. M1B human pilot, M0 CI/signing/credential gates remain open. No release; version stays 0.0.9.
