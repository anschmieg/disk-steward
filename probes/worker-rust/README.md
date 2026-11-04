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

The shared stdout reader accepts readiness only when the first byte is exactly
`R`. Other first bytes fail the reader without emitting readiness. Empty EOF
remains a zero-byte completion for the fixed partial-write failure case.
Interrupted reads have a cumulative allowance of sixteen; the seventeenth
fails, and successful reads do not reset the allowance. These checks bound
protocol handling, not the duration of a blocking OS read or arbitrary peers.

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

`--run-store-payload-fixture` creates one fresh private store and exclusively
creates its fixed `payload.bin`. It attempts 64 KiB of deterministic nonzero
fixture data in chunks no larger than 4 KiB, stopping at a 32 KiB logical cap.
Before payload creation and each chunk it samples free space on the root FD,
requiring the 2 GiB minimum reserve plus probe-specific 1 MiB headroom and the
pending bytes. It checks the named/held file identity, regular-file metadata,
private ownership and current length before writing. There are no caller path,
size or filename parameters. Extra arguments and cleanup combinations refuse
before work begins.

This command retains all artifacts, including partial or uncertain outcomes on
write/sync failure, and returns no store handle or cleanup receipt. The owner
journal format and existing cleanup logic are unchanged: that cleanup refuses
the added payload entry before mutation. The result measures bounded logical
writes, payload allocation observations and sampled reserve gating only, not a whole-store aggregate
quota, Git object writes, a continuously maintained reserve against other
writers, calibrated production headroom or power-loss durability.

The payload command now uses the shared budget controller for advisory
admission before creation and each chunk. On Linux/macOS it charges the
monotonic high-water mark of the greater of logical payload length and reported
allocated bytes (`MetadataExt::blocks()` in 512-byte units, checked for overflow).
It observes the held payload before/after writes and after synchronization,
including failed writes or syncs. Lower later observations never release a
previous charge. This conservatively retains observed partial usage without
claiming that allocated blocks equal uniquely owned or reclaimable bytes. Completed fixture jobs retain their charged bytes with
cleanup unconfirmed; other incomplete store operations fail the fixture job
without asserting worker-stop proof. An existing observation-limit failure
keeps its original reason. Unknown post-write free space is treated
conservatively as zero for post-write accounting; unreadable or overflowing allocation metadata leaves
an uncertain retained fixture rather than a claimed complete observation.
The controller is local to this command, so this is not durable/global
aggregate enforcement across retained fixture runs.

This accounting covers payload-file growth only; owner/control metadata are
not included in its observed byte total. Their production storage accounting
and persistence across retained stores remain separate gates.

Block counts follow [Rust's Unix metadata contract](https://doc.rust-lang.org/std/os/unix/fs/trait.MetadataExt.html#tymethod.blocks).
They can differ from file length for sparse or compressed files. Shared extents,
delayed allocation and filesystem-specific reclamation still need separate
validation; a sampled high-water charge is not a disk-space reservation.

The indexed-worker fixture also records a fixed 128 KiB admission reservation
before starting its child, then verifies that the exact value survives worker
stop and registry reopen. Reopen must refuse further reservation growth until
reconciliation. This is an admission-bookkeeping exercise, not a measurement of
the fixture's footprint or proof of global aggregate enforcement. The probe
retains its per-store private control layout and grants no cleanup authority.

All four fixed current-executable child launch paths clear the caller environment
before spawning. They need no ambient PATH, Git configuration, credentials or
caller-selected temporary directory. A fixed synthetic sentinel is rejected by
the private child dispatcher without printing its value. Integration tests first
prove that direct inheritance is detected, then run stdout supervision, store
worker, creator-death reinspection and indexed store worker with that sentinel
set in their parent. This checks the actual launch boundary, not OS-level process
isolation or production transport integration. Local evidence is macOS; other
supported-platform behavior requires its current-head CI.

## Descriptor-only store handoff

`cargo run -p disk-steward-worker-probe -- --run-indexed-store-handoff`
uses the indexed-store fixture with an additional fixed capability handoff. It
commits the same identity-bound 128 KiB registry reservation before spawn, then
passes a one-shot clone of the verified store directory as the exact child’s
stdin. The child receives no path or descriptor-number argument.

Before emitting its readiness byte, the child checks directory type, private
owner/mode, root device/inode and a bounded regular ownership journal naming its
own PID. The initial parent bind may be observed as a created or partial journal;
only valid prefixes may wait, for at most 256 polls and 500 ms. Reads are bounded
to 513 bytes with at most 16 interruptions, and the named journal is reopened to
reject replacement. Changed, malformed or foreign identities refuse readiness.
A failed handoff attempt cannot be renewed by restoring marker permissions.

The child performs read-only validation and the existing fixed stall. After
readiness the parent kills/reaps the exact child, joins its reader, records
stopped state, and checks the reopened registry remains reconciliation-required.
Only then does the report include `descriptor_handoff=true`. The fixture and
reservation remain retained; no cleanup, payload write, Git access or credential
operation is authorized by this mode. Existing fixed modes keep their behavior.

Passing a directory descriptor is not an OS sandbox or process-tree containment.
Cooperative deadlines and ordinary local kill/wait/join evidence do not prove
hard latency under OS stalls, atomicity against same-user interference, power-loss
durability or production verification ownership. Linux/macOS paths are supported
by the probe; this change was locally exercised on macOS arm64 and still needs
current-head cross-platform CI.

## Fixed payload through the worker descriptor

`cargo run -p disk-steward-worker-probe -- --run-indexed-store-payload`
adds a separate fixed synthetic write to the directory handoff. The parent
commits the same 128 KiB identity-bound reservation before launch. The child
validates its bound directory capability, then writes only `payload.bin` with
the existing deterministic 32 KiB generator through the shared accounting core.
That core preserves native free-space sampling, the 2 GiB reserve plus 1 MiB
headroom, admission checks, allocation-aware high-water charges, partial-write
accounting, exclusive no-follow creation, identity checks and synchronization.
The original in-process payload mode uses that same core.

Readiness requires the full fixed write. After exact child shutdown and reader
join, the parent requires the matching stop receipt and stopped journal before
readback. It checks all bytes through a held descriptor, exact file metadata,
size and identity, named-file identity and metadata, EOF, and final unchanged
metadata. Readback has a cooperative one-second deadline and bounded reads and
interruptions. The report requires payload high-water usage within the committed
reservation; reopening the registry must still yield reconciliation-required
state with the reservation retained. Neither success nor failure deletes the
payload or authorizes cleanup.

The command accepts no path, data, FD number, endpoint, credential or cleanup
argument. Missing/wrong capabilities, repeat writes, changed journals/files and
unconfirmed worker stop refuse acceptance. This is synthetic worker/store
composition only: no Git transfer, durable byte receipt, directory/index
footprint accounting, OS hard latency/process-tree/RSS guarantee, private
authentication, atomicity against same-user interference or milestone completion
is established. Current-head platform CI remains required.

## Partial payload failure before readiness

`cargo run -p disk-steward-worker-probe -- --run-indexed-store-payload-partial`
checks a separate fixed failure path. The environment-cleared child receives
only the identity-bound store directory, writes a deterministic 2 KiB prefix
through the shared payload accounting core, then injects a write error. It exits
with a fixed failure status and emits no readiness bytes. This does not simulate
a real disk-full error or arbitrary kill timing.

The parent requires bounded empty stdout completion, exact child shutdown and
reader join, and the expected exit status before inspecting the partial file.
The complete-payload inspector must reject it. A separate fixed partial
observation checks the prefix, EOF, private metadata, held and named identity,
and unchanged metadata under the same cooperative readback deadline. The
observation is evidence of an incomplete attempt, never successful verification.

The 128 KiB reservation remains retained. Because readiness never succeeded,
the registry never becomes Active; reopening requires reconciliation with prior
Registered state and refuses retry and reservation growth. The file remains in
the task-owned fixture. No cleanup authority or source-repository access is
added. This is process-exit evidence, not synchronized power-loss durability,
production worker isolation, global store accounting, credential validation or
milestone acceptance.

## Worker reservation binding

The indexed complete and partial payload fixtures bind the fixed 128 KiB
registry reservation into the owner journal after admission succeeds and before
handing the directory to the child. The once-only binding is available only in
the fresh store phase. The child requires that exact reservation together with
its PID, root identity and unchanged bound journal before creating the payload.
The descriptor-only and stall modes retain their unreserved journal behavior.

The worker writer uses the bound ceiling as an additional check around the
shared accounting core: high-water usage plus the next chunk must fit before
writing. Actual allocation growth is charged before an over-cap observation
refuses further work, including after synchronization. No readiness is emitted
on failure; partial bytes and the durable reservation remain retained. The
parent still requires stopped-worker identity-bound readback and registry reopen
reconciliation checks.

This binds a fixed synthetic payload operation to its precommitted admission.
It does not measure directory/index footprint, impose a filesystem quota or
prevent allocation growth between observations. It adds no cleanup, schema,
renderer access, arbitrary reservation input, production aggregate ownership,
credential acceptance or milestone-completion claim.
The child validates a journal claim from the trusted supervisor, not the SQLite
row itself. The closed supervisor is responsible for ordering and checking the
registry admission before journal binding. Same-user mutation races and dynamic
reservation revocation are not established by this immutable fixture.

The partial-payload fixture also persists `NeedsReconciliation` through the
live registry before reopening it, after confirmed child shutdown and retained
prefix inspection. It checks immediate reservation-growth and lifecycle retry
refusal, then requires reopening to preserve the same identity, prior Registered
state and 128 KiB reservation. `reconciliation_latched=true` reports this
bookkeeping check; it is not a completed reconciliation or cleanup receipt.

## Fixed six-object footprint observation

The complete indexed payload fixture observes its known parent/store
directories, owner journal, payload, registry database and lease after confirmed
worker shutdown and validated payload readback. It requires exact entry lists,
private metadata, distinct identities, held/named agreement and unchanged
metadata across the observation. Unknown entries, SQLite sidecars or changed
objects refuse the result. Observation uses checked sums of the larger of each
object's logical length and native allocated blocks, with cooperative deadlines.

The report includes the six-object byte total and count and checks the snapshot
against 128 KiB. That is a separate fixture observation: the existing reservation
still admits the payload, and this does not establish a whole-store quota or
lifetime allocation peak. Partial and descriptor-only modes do not claim this
observation. Files and reservations remain retained; no cleanup eligibility,
recovered-space claim, production aggregate accounting, OS containment or
milestone acceptance is added.
The report labels this as `fixture_snapshot=before_registry_reopen`. Reopening
and later SQLite writes can change control-file allocation after the snapshot;
the value is not a final or maximum footprint for the probe lifecycle.
