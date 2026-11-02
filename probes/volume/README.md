# Controlled APFS space recovery probe

Development-only and opt-in: `python3 probes/volume/space_recovery_probe.py`
prints a dry-run plan. `--run` is required to create and mount a private 4 GiB
APFS image, write a fully allocated 256 MiB incompressible fixture, simulate
stage/restore/stage, and purge that exact fixture. Before image creation it
requires at least 6 GiB free on the host volume (4 GiB image budget plus a 2 GiB
reserve). It records free space on the mounted test volume at each step and
observes for up to 30 seconds after purge. Success requires observed recovery
within `max(16 MiB, 10% of allocated fixture bytes)` of the actual `st_blocks`
allocation. No recovery within that tolerance is reported as a failed probe
with a nonzero exit code.

The probe only knows its private temporary root, image, mountpoint, and file.
It never examines or removes snapshots, real user files, or unrelated paths.
It detaches only the device returned by `hdiutil attach` for its mountpoint.
If attach fails or times out without a trustworthy device receipt, or detach
fails/times out, it retains the image/root and reports their location rather
than deleting a possibly mounted volume. These states never count as a passing
probe. A failed live run may therefore need manual review of the printed
receipt before the retained probe directory can be removed.

Unit tests validate command construction and receipt/path containment without
invoking `hdiutil`. This does not establish production journaling, crash
recovery, APFS behavior on supported macOS versions, or the production free
space reserve policy. No live run is performed as part of ordinary tests.
