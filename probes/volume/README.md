# Controlled APFS space recovery probe

Development-only and opt-in: `python3 probes/volume/space_recovery_probe.py`
prints a dry-run plan. `--run` creates and mounts a private 768 MiB APFS image,
writes a fully allocated 256 MiB incompressible fixture, and simulates
stage/restore/restage. Purge additionally requires both
`--purge-own-fixture --confirm-purge "PURGE ONLY THIS PROBE'S FIXTURE"`.
Before image creation it requires a 2 GiB host-volume reserve plus image
budget and fixture overhead (3 GiB total). On the mounted experiment volume,
it requires room for the fixture plus a separate 64 MiB free margin; that
margin is an experiment constraint, not a production reserve policy. A live
run requires all explicit purge flags before any image is created. It records signed free-space
deltas at each step and observes for up to 30 seconds after purge. Success
requires observed recovery within `max(16 MiB, 10% of allocated fixture bytes)`
of the actual `st_blocks` allocation. No recovery within that tolerance is
reported as a failed probe with a nonzero exit code.

The probe only knows its private temporary root, image, mountpoint, and file.
It never examines or removes snapshots, real user files, or unrelated paths.
It detaches only the device returned by `hdiutil attach` for its mountpoint.
If attach fails or times out without a trustworthy device receipt, or detach
fails/times out, it retains the image/root and reports their location rather
than deleting a possibly mounted volume. These states never count as a passing
probe. A failed live run may therefore need manual review of the printed
receipt before the retained probe directory can be removed.

Unit tests validate command construction, receipt/path containment, pre-create
authorization, experiment-volume margin, exact confirmation parsing, and
signed recovery tolerance without invoking `hdiutil`. This
does not establish production journaling, crash
recovery, APFS behavior on supported macOS versions, or the production free
space reserve policy. No live run is performed as part of ordinary tests.
Even a successful native probe only supplies controlled-volume feasibility
evidence; it does not close M1C's product-adapter measurement gate.


## Live evidence — 2026-11-02

The reviewed probe ran successfully on macOS 27.0 arm64 with its own 768 MiB APFS image and 256 MiB random fixture. The fixture's allocated size was 268,435,456 bytes. The signed free-space change after exact-fixture purge was 268,443,648 bytes, within the 26,843,545-byte tolerance; stage, restore and restage each produced zero measured free-space change. The configured observation window was at most 30 seconds, not a claim that reclamation took 30 seconds. The transient device returned for this run was detached, and the owned image and temporary root were removed; a separate `hdiutil info` check confirmed no matching image remained.

The normalized [evidence record](evidence/2026-11-02-macos-arm64.json) omits temporary filesystem paths and records measurement method and platform. The original report was retained outside the repository for local inspection. This is calibration evidence for filesystem semantics, not an implementation of observed-space reporting in the product adapter, nor a human usability or external-volume validation pass.

## Native product observation probe

`python3 -m probes.volume.native_observation_probe` prints a dry-run plan by
default. On macOS, a live run requires both `--run
--confirm-own-demo-purge --confirm-own-calibration-unlink` before it creates a
private 768 MiB APFS image. It uses only the fixed
`probes/fixture-migration/target/debug/desktop_migration_harness` helper, with
a fresh marked HOME on the mounted volume. The helper seeds its own demo,
calibrates the native `fstatvfs_bavail_v1` sampler with an exclusive 256 MiB
file, and purges only its own staged demo selection through the real product
coordinator. The probe requires a finished result whose selected logical
bytes all reached durable unlink, plus a product volume observation with the
expected method and a 64-character binding digest. It records platform,
calibration JSON and product JSON. The purge observation is evidence about the
owned volume; its measured delta is not attributed to the purge's logical
bytes. Host free space must be at least 3 GiB before image creation.

The probe detaches only the device returned for its exact mountpoint. Unknown
attach state and failed detach retain the image and root for inspection. A
successful detach permits removal of the named image and empty directories.
This is development-only feasibility evidence and does not authorize ordinary
volume cleanup or unattended deletion. No live run is performed by tests.


### Verified native method and product path — 2026-11-02

The 0.0.16 native runner passed on macOS 27.0 arm64. Its 256 MiB allocation was measured through the actual opened-directory Rust sampler; the native calibration met tolerance immediately in the first sampled interval (reported elapsed milliseconds rounded to zero). The real demo coordinator then returned a finished purge result with complete logical unlink and a separate signed observation. These numbers are independent: the observation spans volume activity and control-store writes as well as file unlink.

The [normalized evidence](evidence/2026-11-02-native-observation-macos-arm64.json) preserves both calibration and product results, omits temporary paths, and records independent detach/root-removal checks. The fixed demo files were not enlarged to perform calibration. Missing authorization and calibration on the host temporary filesystem were separately refused before calibration writes. Runtime probe artifacts were removed after the passing run; failure cases retain evidence without forced detach or recursive cleanup.
