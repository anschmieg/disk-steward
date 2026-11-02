# M0 durable verification-store metadata index

Purpose: provide a backend-owned index for startup classification without enumerating user directories. Records contain fixed-format store IDs, immutable device/inode identities and lifecycle bookkeeping only; no source paths, credentials, filesystem mutation, process execution, renderer SQL or cleanup/adoption permission.

Private SQLite control database and exclusive process lease; unknown schema/object/version or unsafe control files refuse. On exclusive reopen, unfinished records become NeedsReconciliation while preserving identity/prior state. Completed records remain terminal. Reconciliation is never a transition back to active. Bounded records/listing and exact expected-state transitions prevent silent replacement or stale updates. Bookkeeping cannot substitute for actual worker/store identity and shutdown proof.

Root reviews concrete code before new fixture tests. Tests create their own private control directories/databases only; no real app/user store opens. Relevant tests/fmt/strictClippy/version and current-head CI before merge. Version0.0.17, mainbaselinef2a9e35. Preservepending31integrationnote. This is unintegrated control metadata, not production startup reconciliation or full M0/Git/authentication completion.
