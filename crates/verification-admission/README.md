# Verification admission

This backend-only helper composes a process-local verification budget with an
already leased durable store registry. `admit_registered_store` accepts only a
fresh Registered row with zero reservation and no prior state. It rejects
active, completed, reconciled and previously reserved stores rather than adopting
or resuming an attempt. Requested storage must be positive and within the
existing per-repository limit.

A successful result follows three steps: start the local running slot, preflight
the full requested storage against local aggregate/free-space limits, then
commit the exact registry reservation using expected current zero. The result
contains a job ID and committed metadata, not a filesystem or execution
capability. This crate opens no stores or payload files, starts no worker,
performs no network requests and exposes no renderer command. Registry metadata
I/O stays behind the existing validated API.

Known refusals before handoff retain failed/canceled budget history and
acknowledge that this helper launched no worker, without confirming cleanup.
Ambiguous registry storage/path/schema outcomes and mismatched committed
records leave the local job incomplete with its slot retained and return no
successful result. The helper never rolls back a committed reservation, deletes
a store, retries, adopts or reconciles it. Callers remain responsible for
confirmed shutdown and conservative reconciliation after later handoff failure.

Coordinated admissions refuse once the supplied budget contains 4,096 retained
jobs, including failed and completed jobs. The helper never prunes or resets
history. Callers must retain and reuse the intended budget and leased registry;
this function does not create an app-wide owner or prevent a trusted backend
from bypassing it with unrelated controllers. The separate budget-controller
history-cap work remains independent.

The numeric free-space value must come from the caller's validated volume and
can become stale. Actual runtime growth still needs accounting; SQLite
reservations are metadata admission, not a filesystem quota. The fixed indexed
worker parent uses this helper before journal binding and directory handoff,
after its initial root/control creation. Its admission budget does not replace
child runtime accounting, prove initial creation was admitted, or establish
whole-store peaks, durable measured-byte totals or production global ownership.

Tests use private disposable control fixtures and numeric large totals, never
GiB-sized files. Synthetic injected errors cover retained-slot and committed
reservation behavior; they do not establish hardware power-loss outcomes.
