# Fixture lifecycle migration probe

This probe has two layers:

- The existing test crate covers exact v1-to-v2 SQLite rebuilding, rowid and row preservation, malformed/future schema refusal, and transaction fault rollback.
- `desktop_migration_harness` uses the public `DemoExecutionService` API to seed a five-fixture session, stage/restore/stage it, freeze that owned lifecycle DB to the v1 schema, then verify migration and receipt reuse. It accepts exactly one command: `seed`, `status`, or `verify`; paths are derived from `HOME`. `HOME` must be a non-symlink directory under the system temp directory and contain `.disk-steward-disposable-probe` with the exact marker value. The fixed app-data directory is `Library/Application Support/com.disksteward.migration-probe` and must not exist before `seed`. The matching Tauri override is `probes/fixture-migration/tauri.migration-probe.conf.json`; this keeps the probe away from the ordinary installed app-data identifier.

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

Use the status step only for a disposable HOME freshly seeded by this helper. It calls `DemoExecutionService::status`, which lazily reopens and migrates the fixture session. `verify` is read-only with respect to the lifecycle DB and refuses to run before the version is already 2. These three migration modes do not delete fixture files. The explicit measurement modes below remove only their own calibration file or registered staged demo receipt; no mode removes user files. Keep the temporary HOME until evidence has been inspected; remove only that exact disposable directory afterward.

## Desktop integration boundary

`DemoExecutionService::open` initializes the coordinator during Tauri setup, but it does not reopen the fixture lifecycle DB. Migration occurs when `get_fixture_demo_status` calls `status`; the renderer currently makes that call when the fixture demo panel mounts. Opening the app on its default page is therefore not sufficient evidence.

A desktop pass must launch the debug executable with the marked disposable HOME, open the **Fixture demo** panel, close that same process, then run the helper's `verify` command and observe schema version 2. CUA selection by the generic display name “Disk Steward” is ambiguous on a machine with an installed app; it must not be used as proof that a particular debug executable was selected. In the latest attempt, the disposable DB remained at version 1 after the UI interaction, so the desktop integration result is **inconclusive**. The successful `status` then `verify` run proves the public coordinator path on the seeded disposable session, but not that the running desktop process triggered it.

This probe does not define a production migration entry point, a renderer seeding capability, a generic filesystem/SQL command, or a cleanup operation. The existing app runtime remains responsible for migration under its owner lease.

## Exact-instance desktop verification (macOS)

Use the unique configuration override for both compilation and execution. Do not launch the production bundle or select an app by the generic `Disk Steward` name. First seed and check the migration probe's app-data path using a fresh marked HOME, then build the debug app with the override:

```sh
probe_home="$(mktemp -d "${TMPDIR:-/tmp}/disk-steward-migration.XXXXXX")"
chmod 700 "$probe_home"
printf 'disk-steward-migration-v1\n' > "$probe_home/.disk-steward-disposable-probe"
target_dir="${TMPDIR:-/tmp}/disk-steward-migration-target"
CARGO_TARGET_DIR="$target_dir" cargo build --locked --manifest-path probes/fixture-migration/Cargo.toml --bin desktop_migration_harness
HOME="$probe_home" "$target_dir/debug/desktop_migration_harness" seed
npm run tauri -- build --debug --bundles app --config "$PWD/probes/fixture-migration/tauri.migration-probe.conf.json"
```

Tauri's merged override sets the identifier to `com.disksteward.migration-probe`, product name and window title to `Disk Steward Migration Probe`. Before UI automation, identify the binary produced under `src-tauri/target/debug` using its resolved executable path and inspect the process executable path and open control-store files beneath the marked HOME. Confirm the app-data path under that HOME contains the seeded `com.disksteward.migration-probe` state and that the process is the probe build. The path and identifier are the instance proof; the display name alone is not.

Launch that exact executable with `HOME="$probe_home"`, interact with its **Fixture demo** panel so `get_fixture_demo_status` runs, and close that same process. Then run:

```sh
HOME="$probe_home" "$target_dir/debug/desktop_migration_harness" verify
```

This probe's appdata identity is distinct, but Tauri may derive platform paths using macOS conventions in addition to `HOME`; confirm the actual `app.path().app_data_dir()` behavior from the seeded root before launch. Do not use CUA's ambiguous display-name app selector. Keep the marked HOME until evidence is reviewed, then remove only that exact disposable directory manually. Desktop verification remains unproven until a process-path/identifier match and post-UI version-2 verification are both recorded.

Run crate tests with:

```sh
CARGO_TARGET_DIR=/private/tmp/disk-steward-migration-target cargo test --manifest-path probes/fixture-migration/Cargo.toml
```

## Verified desktop pass — 2026-11-02

The probe bundle was built with `--debug --bundles app` and the override above. Its `Info.plist` identifier was verified as `com.disksteward.migration-probe`; its title and product name were `Disk Steward Migration Probe`. The exact bundle executable was launched with a fresh marked HOME. The recorded PID's command matched that bundle, and `lsof` showed that PID holding control stores beneath the seeded probe app-data root. The CUA inventory showed the unique identifier running and the ordinary `com.disksteward.desktop` app stopped. CUA then selected the unique identifier, not a display name.

Before opening **Fixture demo**, a read-only SQLite query found `user_version=1`. After the panel mounted and showed the staged Compiler output fixture, the same database reported version 2. The recorded process was terminated by its verified PID; the verifier then passed all row/rowid and fixture-byte/digest comparisons and prepared a restore review using the preserved receipt. Stderr was empty. No `status` helper call was used between seeding and desktop verification. This proves the real desktop status-triggered migration boundary for the isolated probe build; installed production data was not needed for this pass. The earlier ambiguous attempt remains inconclusive historical evidence.


## Native observation measurement modes (0.0.16)

Build the helper under the ordinary developer HOME before running `python3 -m probes.volume.native_observation_probe`. The runner creates and owns the image, mountpoint and marked probe HOME; it accepts no arbitrary image, device, helper or data path. A live run requires `--run --confirm-own-demo-purge --confirm-own-calibration-unlink` before any image creation. Host reserve and exact returned-device detach rules are shared with the existing volume probe.

`calibrate-own-volume --confirm-own-calibration-unlink` requires the seeded probe root on a distinct mounted filesystem. It exclusively creates a fixed calibration filename, writes and syncs 256 MiB of OS-random data, verifies allocation and identity, removes that exact file, and observes the actual lifecycle Rust sampler for at most 30 seconds. It refuses the host temporary filesystem. It does not change fixture eligibility or use a registered fixture as the large calibration file.

`purge-own-staged-demo --confirm-own-demo-purge` reads only the receipt saved by `seed`, requires that exact receipt still staged, prepares its exact current review and supplies the trusted test confirmation only after the explicit CLI flag. It invokes the real coordinator and emits its result, including any optional volume observation. It does not add a production command, bypass native confirmation in the app, or accept a user-file path. See the [native observation runner](../volume/README.md#native-product-observation-probe) and its normalized evidence for the controlled-volume check.
