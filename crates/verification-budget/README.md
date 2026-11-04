# Verification budget accounting

This crate tracks the explicit ingress, temporary-storage, aggregate-storage,
free-space, elapsed-time and concurrency limits for verification jobs. It performs
no I/O. Callers supply observations and confirmed worker/cleanup outcomes; these
state transitions do not grant filesystem or cleanup authority.

A controller retains at most `MAX_RETAINED_JOBS` (4,096) job snapshots, including
completed and cleanup-confirmed jobs. At capacity, `start` returns
`Blocked(RetainedJobLimit)` without advancing the job ID, changing existing
snapshots, or releasing accounted store bytes. An existing worker still takes
precedence through `AlreadyAtConcurrencyLimit`. History remains retained; no
eviction, reset or automatic retry is provided.

This ceiling bounds retained job count, not total process RSS. Durable history,
store ownership, global admission and restart reconciliation require integration
with the worker and registry. Constructing a fresh controller does not prove
that previous stores were cleaned or release their durable reservations.
