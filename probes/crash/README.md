# Disposable crash-boundary reconciliation probe

This developer-only probe interrupts a fixed child writer at two checkpoints:
after the stage intent and consistency receipt have been fsynced but before
rename, and after rename plus parent-directory fsync but before the outcome
journal is written. Running `python3 probes/crash/crash_probe.py` is a dry run;
`--run` is required to create its own temporary fixture roots and child
processes. The CLI accepts no root, file path, or command.

The child moves one synthetic 128 KiB fixture between fixed `original/` and
`staged/` slots. Journal paths are validated as fixed slot names and are never
used to construct filesystem paths. Reconciliation reads the intent and
receipt, fingerprints both fixed locations, and reports `original`, `staged`,
or `uncertain` without retrying the move. Restore is a separate explicit
fixture-only operation. Changed content, an original-slot collision, changed
parent-directory identity, or a mismatched receipt blocks restore. The receipt
digest is an unkeyed consistency check, not an authentication mechanism.

Cleanup is limited to roots made by the private factory and registered with
fresh device/inode identities for the root, its parent, and both fixture
directories. It also requires a supervisor receipt for the exact joined child.
If stop confirmation or identity validation fails, the probe retains the root.
Directory fsync is required; `--run` reports unsupported on platforms where
Python does not expose it.

Run `python3 -m unittest probes.crash.test_crash_probe -v` for focused tests, or
`python3 -m unittest discover -s probes -p 'test_*.py'` for the probe suite.

This demonstrates local temporary-file staging and process-interruption
reconciliation only. It does not prove power-loss behavior, native Trash,
race-free path replacement, cross-platform behavior, or production cleanup
authority. `os.rename` is used only for this disposable fixture after a
collision check; path races remain unproven.
