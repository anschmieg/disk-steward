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
Inspection always returns `NeedsReconciliation`; the ordinary store commands leave the fixture in place.
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
after restart. Journal updates
validate the prior contents and inode/mode, use the fixed descriptor-relative
marker, and poison the live handle after any update error. An interrupted
in-place update may leave a malformed journal requiring manual reconciliation.

`--run-store-cleanup --confirm-own-store-cleanup` runs that same fixed child
supervision and stop receipt path, then removes the recorded `owner.journal`
through the opened root descriptor and the fixed `store` directory through the
opened private parent descriptor. It requires a live matching token/PID receipt,
the stopped phase, exact marker and directory identities, and directory listings
containing only the expected entries. Before unlinking, it exclusively creates
and synchronizes `cleanup.journal` in the parent with a pending record. After
the root is removed and the parent synchronized, it records completion and
retains that parent journal permanently. Once the receipt gate passes, any cleanup failure poisons the live handle;
there is no retry, reopen adoption, recursive deletion, or parent deletion.
The command reports that the root was removed and names the retained journal;
it does not estimate recovered bytes.

`--run-store-reinspect` starts one copy of its own executable in the fixed
`store-abandon` child mode. The child creates its own private fixture, binds its
actual PID, emits a versioned path-free record capped at 1024 bytes, flushes,
and exits without dropping the store handle. The parent captures only that
bounded record within a one-second capture deadline, stops/reaps the creator,
joins the reader, checks the child PID
and record shape, then reopens only the canonical token-derived fixture using
no-follow private-directory and journal identity checks. It reports
`NeedsReconciliation` with `creator_stopped=true` and
`cleanup=not_authorized`. This is ordinary process death, not power-loss or
crash-durability evidence. It does not adopt the store or create a stop receipt.

`--run-indexed-store-worker` integrates the local verification-store registry
with a fresh private store. It creates the registry database only inside that
store's generated private parent and uses one fixed 32-hex fixture ID. It binds
the identity as `registered` before spawning one actual `stall` child, then
records `active` after spawn and binding. After readiness, the bounded one-byte read, stop, reap and reader
join proof, it records `stopped` in both live store and registry. It drops the
registry lease and reopens only the generated registry path, then verifies the
same parent/root identities with `NeedsReconciliation` and prior state
`Stopped`; retrying `Active` is refused. The index grants no inspection or
cleanup authority, and the command accepts no paths or IDs.

Descriptor-relative checks constrain the operation to this fixture, but they
cannot eliminate races from a hostile process running as the same user between
validation and unlink. Directory and file synchronization requests also do not
prove power-loss durability across every filesystem or operating system. This
probe grants no generic cleanup authority and never accepts user paths.

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
cargo run -p disk-steward-worker-probe -- --run-store-reinspect
cargo run -p disk-steward-worker-probe -- --run-indexed-store-worker
cargo run -p disk-steward-worker-probe -- --run-store-cleanup --confirm-own-store-cleanup
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
