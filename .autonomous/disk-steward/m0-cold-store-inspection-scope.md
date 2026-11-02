# M0 read-only store inspection after creator process death

Purpose: replace live-handle-only evidence with a real fixed creator subprocess whose app-owned fixture survives its exit. No paths accepted. Child creates its own store, records its actual PID, emits a bounded path-free versioned token/identity record, and exits without automatic cleanup. Parent boundedly captures the record, stops/reaps/joins child, validates actual PID and strict record syntax, then opens fresh no-follow descriptors below its canonical temporary anchor and validates private metadata, identities and exact journal bytes.

Inspection returns NeedsReconciliation only. A separate immutable inspection record cannot become OwnedStore or mint StopReceipt, cleanup, mutation, retry or adoption authority. No generic child commands, executable/path inputs, network, credentials, renderer access, source files or releases. Only freshly created fixture metadata is read/written; all artifacts retained.

Parent reviews concrete source before any new probe/test execution. Validate actual process-death path, bounded parsing/capture and malformed/future/traversal records; run fmt/strictClippy/version/diff checks. This is OS process-death, not power-loss proof, production startup reconciliation or process-tree containment. Version.17; basedintegratedmain13a2ef2, preserve28/29/30integrationnotes. Mainfinaltarget.
