# Disposable child-writer resource probe

This development-only probe measures whether a parent process notices growth
past a small fixture byte budget and enforces a wall-clock deadline on a fixed
synthetic child writer. Running `python3 probes/worker/worker_probe.py` is a
dry run: it creates no temporary files and starts no child process. `--run` is
required to create private temporary stores and execute the two bounded cases.

The child is fixed code. It accepts no user path, command, URL, or input file;
it can only exclusively create one synthetic file with fixed 4 KiB writes.
The byte case stops at a 256 KiB fixture cap with a 64 KiB observed-allocation
budget. The deadline case uses a 1 MiB fixture cap and a 0.15 second hard
deadline. The parent samples allocated blocks (`st_blocks * 512` where
available), stops and joins the child before removing its private store, and
prints observed allocation growth, byte overshoot, elapsed time, and deadline
overshoot. If child exit cannot be confirmed, the store is retained. Allocation
overshoot varies with parent scheduling and filesystem block allocation: one
coordinator APFS run observed 262,144 B growth against the 65,536 B budget,
an overshoot of 196,608 B, below the 262,144 B fixture cap. Another run
observed 204,800 B growth, an overshoot of 139,264 B. These are fixture
measurements, not stable enforcement limits.

Unit tests exercise path ownership, rejection of cleanup without a matching
joined-child receipt, bounded byte-budget failure, and timeout/overshoot
reporting. Tests use only stores created by the probe and do not launch a live
user workload.

This validates only a local Python process-supervision fixture. It does not
prove production worker isolation, cross-platform filesystem allocation
accounting, strict scheduling guarantees, race-free path replacement handling,
durable journaling, or enforcement for Git/network verification. Production
resource controls remain unproven.
