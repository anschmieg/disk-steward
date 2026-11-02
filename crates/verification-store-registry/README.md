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
Reconciliation is intentionally outside this M0 API.

Supported filesystem enforcement is limited to Linux and macOS. Other targets
are refused. Paths must come from a trusted backend; this is not a renderer API.
This foundation is not yet integrated with store creation, workers, or the
application lifecycle, and does not establish milestone completion.
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
