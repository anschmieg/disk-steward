# Verification Store Registry

This crate provides a bounded, durable SQLite index for trusted backend
verification-store bookkeeping. It records opaque caller-provided IDs and
lossless device/inode identity metadata. It does not discover, adopt, inspect,
create, or clean up stores, and its records grant no permission to access data.

Opening the registry requires an existing private control directory and a
private regular database file (or creates the database with private mode). A
nonblocking process lease prevents simultaneous registry owners. On every
successful open, unfinished records are classified as
`needs_reconciliation`; this crate never retries work or declares it complete.
Reconciliation is intentionally outside this M0 API. The lease guard explicitly
unlocks after the SQLite connection closes, so a briefly inherited or duplicated
file descriptor cannot extend normal owner-drop lifetime. Closing such an old
duplicate cannot unlock a subsequent owner's independently opened lease. If
unlocking fails, ordinary close remains the fallback and acquisition still
fails closed; no retry or ownership bypass is introduced.

Supported filesystem enforcement is limited to Linux and macOS. Other targets
are refused. Paths must come from a trusted backend; this is not a renderer API.
The closed indexed-worker probe exercises registration, reservation and reopen
bookkeeping around an owned child. The application lifecycle is not integrated,
and this foundation does not establish milestone completion.
It assumes a trusted backend supplies the control path and makes no claim of
protection against hostile path replacement by the same user.

The index holds at most 4,096 records and returns keyset pages of at most 256.
IDs are 32 lowercase hexadecimal characters; device/inode values round-trip
the full unsigned 64-bit range. Identity metadata cannot be replaced by a
duplicate registration. Live lifecycle transitions are linear; reopening
preserves the last nonterminal state as `prior_state`, and repeated reopen
does not erase it. Lifecycle values are bookkeeping observations, not proof
that a worker stopped or a store was removed.

Every operation validates the named control files, schema and bounded row
contents within a transaction. The validated private parent is canonicalized
while the database leaf retains no-follow protection, including on macOS paths
with a system directory symlink. The Linux/macOS implementation is locally
verified on macOS; other-platform refusal stubs were not cross-compiled.
This does not protect against a hostile same-user actor racing path validation.

Schema v2 adds `reserve(id, expected_current, new_total)`. Under the exclusive
lease and an immediate transaction, it admits nondecreasing reservations on
live registered/active rows, up to 2 GiB per store and 4 GiB across every record
in that registry. Cleanup-complete reservations remain counted. A stale
expected value, limit violation, malformed record or reconciliation requirement
refuses without changing the reservation. There is no release API.

Reservations are caller-supplied admission metadata, not measured allocation or
reserved filesystem capacity. Callers must persist admission before writes,
observe actual growth, maintain volume reserve/headroom, and reconcile crashes
and stores outside their current enumeration. The probe currently uses one
registry per private store parent; this is not app-wide aggregate enforcement.
A shared control registry and trusted store lifecycle integration remain gates.

Opening a validated v1 registry migrates it transactionally, preserving identity
and lifecycle metadata. Every legacy reservation becomes `u64::MAX`, an explicit
unknown-usage sentinel, including rows marked cleanup-complete. Any unknown
sentinel or needs-reconciliation row blocks all new reservations; zero is not
inferred for legacy data. New registrations start with zero admission metadata.
Reservations survive reopen, while unfinished rows become needs-reconciliation.
Unknown/future/malformed schemas are refused before migration. No reconciliation
or release operation is supplied yet, and lifecycle labels grant no filesystem
or cleanup authority.

Linux/macOS process-death integration tests launch an exact ignored child test
under a fresh private task-owned root. The child commits a 128 KiB reservation
and parks while holding the registry lease, either before payload creation or
after writing a five-byte private partial file. The parent verifies the live
lease blocks a second owner, terminates and reaps the child, and joins a bounded
stdout reader before checking evidence. Reopen must retain admission and require
reconciliation; the payload must remain absent or retain exactly those bytes.
These are local process-death tests, not power-loss, kill-latency, global-budget,
production-worker or cleanup-authorization evidence.
