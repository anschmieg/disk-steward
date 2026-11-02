# Fixture lifecycle migration probe

This probe has two layers:

- The existing test crate covers exact v1-to-v2 SQLite rebuilding, rowid and row preservation, malformed/future schema refusal, and transaction fault rollback.
- `desktop_migration_harness` uses the public `DemoExecutionService` API to seed a five-fixture session, stage/restore/stage it, freeze that owned lifecycle DB to the v1 schema, then verify migration and receipt reuse. It accepts only `seed`, `status`, or `verify`; paths are derived from `HOME`. `HOME` must be a non-symlink directory under the system temp directory and contain `.disk-steward-disposable-probe` with the exact marker value. The fixed app-data directory must not exist before `seed`. This prevents opening the ordinary installed app-data path.

The v1 downgrade is harness-only and restricted to the fixture directory just created by the public service. It compares all table rows and rowids before and after downgrade. The migration verifier requires `user_version=2` before examining data, compares every table row and rowid to the saved v2 baseline, compares byte counts and SHA-256 digests for every source and holding file, and checks that the previous staged receipt can prepare a restore review. It does not execute that review.

## Disposable coordinator verification

Create a fresh HOME inside the system temp directory, write the marker, and run these commands from the repository root:

```sh
target_dir="${TMPDIR:-/tmp}/disk-steward-migration-target"
CARGO_TARGET_DIR="$target_dir" cargo build --locked --manifest-path probes/fixture-migration/Cargo.toml --bin desktop_migration_harness
probe_home="$(mktemp -d "${TMPDIR:-/tmp}/disk-steward-migration.XXXXXX")"
chmod 700 "$probe_home"
printf 'disk-steward-migration-v1\n' > "$probe_home/.disk-steward-disposable-probe"
HOME="$probe_home" "$target_dir/debug/desktop_migration_harness" seed
HOME="$probe_home" "$target_dir/debug/desktop_migration_harness" status
HOME="$probe_home" "$target_dir/debug/desktop_migration_harness" verify
```

Use the status step only for a disposable HOME freshly seeded by this helper. It calls `DemoExecutionService::status`, which lazily reopens and migrates the fixture session. `verify` is read-only with respect to the lifecycle DB and refuses to run before the version is already 2. The helper itself never deletes fixture or user files. Keep the temporary HOME until evidence has been inspected; remove only that exact disposable directory afterward.

## Desktop integration boundary

`DemoExecutionService::open` initializes the coordinator during Tauri setup, but it does not reopen the fixture lifecycle DB. Migration occurs when `get_fixture_demo_status` calls `status`; the renderer currently makes that call when the fixture demo panel mounts. Opening the app on its default page is therefore not sufficient evidence.

A desktop pass must launch the debug executable with the marked disposable HOME, open the **Fixture demo** panel, close that same process, then run the helper's `verify` command and observe schema version 2. CUA selection by the generic display name “Disk Steward” is ambiguous on a machine with an installed app; it must not be used as proof that a particular debug executable was selected. In the latest attempt, the disposable DB remained at version 1 after the UI interaction, so the desktop integration result is **inconclusive**. The successful `status` then `verify` run proves the public coordinator path on the seeded disposable session, but not that the running desktop process triggered it.

This probe does not define a production migration entry point, a renderer seeding capability, a generic filesystem/SQL command, or a cleanup operation. The existing app runtime remains responsible for migration under its owner lease.

Run crate tests with:

```sh
CARGO_TARGET_DIR=/private/tmp/disk-steward-migration-target cargo test --manifest-path probes/fixture-migration/Cargo.toml
```
