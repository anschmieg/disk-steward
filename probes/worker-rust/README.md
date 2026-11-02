# Rust subprocess supervision probe

This development-only probe provides a small Rust counterpart to the Python
worker fixture. Running the binary with no arguments is a dry run: it starts
no child. `--run` starts two sequential copies of its own executable using
fixed internal modes. The flood child signals readiness and writes a finite
1 MiB synthetic stream; the stall child signals readiness and sleeps. For the
cancel case, the parent starts that same fixed `stall` child, waits for its
readiness byte, then explicitly selects `Canceled` and enters the shared
stop, reap, and reader-join path. This is synthetic parent-directed
cancellation, not real UI or network cancellation. The
parent reads stdout on a dedicated thread, retains no output bytes, stops
reading after 64 KiB plus one byte, and applies a one-second monotonic deadline.
On either limit it kills the child, waits for its exit, and joins the reader
before reporting success.

The executable path comes only from `current_exe()`. The stdout parent cases accept none or `--run`; the private child dispatcher accepts only the two
fixed modes. These stdout cases have no data-file, network, credential, shell, or renderer
operations, and their children create no descendants.

`--run-store` is a separate development-only store ownership probe. It creates
a fresh exclusive 0700 parent under the canonical temporary directory, then
a fixed-name 0700 `store` root inside that parent, using
directory-relative exclusive creation, writes a small versioned owner journal,
then reacquires directory handles and validates their identities and journal.
Inspection always returns `NeedsReconciliation`; there is no cleanup or retry operation.
This is live-handle validation, not a process-restart journal registry.
The probe intentionally leaves the created directory in place, including on
failure. It does not bind a worker, contain output, measure disk growth, or
prove crash durability. The command does not grant cleanup authority.

`--run-store-worker` creates the same private store, starts exactly one copy of
its own executable in the fixed `stall` child mode, records that PID as bound,
waits for readiness, then cancels, waits for child exit, and joins the bounded
stdout reader. Only after those shutdown checks does the supervisor issue a
private stop receipt and record `stopped` for the same store token and PID.
Inspection still returns `NeedsReconciliation`; the journal is not adopted
after restart and stopped state grants no cleanup authority. Journal updates
validate the prior contents and inode/mode, use the fixed descriptor-relative
marker, and poison the live handle after any update error. An interrupted
in-place update may leave a malformed journal requiring manual reconciliation.

This demonstrates bounded stdout reading and ordinary local child
kill/reap/thread-join behavior. It does not demonstrate Git ingress or store
budgets, production worker isolation, process-tree containment, CPU or memory
limits, or a hard guarantee if the operating system stalls `kill`, `wait`, or
thread joining. The stdout cap is not a network byte budget. No production
cleanup or user data is involved.

Run from the repository root:

```sh
cargo run -p disk-steward-worker-probe
cargo run -p disk-steward-worker-probe -- --run
cargo run -p disk-steward-worker-probe -- --run-store-worker
cargo test -p disk-steward-worker-probe --locked
```

The live probe prints one result per case. The expected flood result is
`stop=OutputLimit bytes=65537`; the expected stall result is
`stop=Deadline bytes=1`; the expected cancel result is
`stop=Canceled bytes=1`. Each result requires a recorded child exit and
`reader_joined=true`. A missing readiness signal, reader failure or unconfirmed
cleanup returns an error instead. Elapsed time includes shutdown and may exceed
the one-second detection deadline; it is not an OS scheduling guarantee.

Both private levels retain pinned identities and require the current UID and
0700 mode before inspection or journal updates. The temporary anchor, parent,
root and journal are synchronized during creation. No automatic cleanup or
restart adoption is introduced.
