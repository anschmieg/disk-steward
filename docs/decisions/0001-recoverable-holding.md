# Recoverable holding for the first lifecycle slice

Status: implementation direction, 31 October 2026. This is an allowed M0 mechanism choice, not a completed production capability or a release-scope reduction.

Use an app-managed, private holding directory on the same validated volume as the source for M1A. Name it recoverable holding in the interface. A move into holding preserves the data and does not count as recovered space. Native Trash remains an investigated alternative; the Foundation experiments do not make it the first production adapter.

The native macOS fixture demonstrated a safe Rust directory-relative no-overwrite rename, stable device/inode through stage and restore, and rejection of final symlinks and malformed component names. This provides a more explicit collision primitive than a check followed by a plain path rename. It still cannot atomically compare a source name to a previously observed identity. Post-isolation revalidation and uncertain-outcome reconciliation remain mandatory.

The backend owns holding-root identity, private receipt directories, original/staged manifests, operation intent and outcomes. It validates opened parent identities and exact targets, persists intent before mutation, and records outcome afterward. A crash never causes an automatic retry. Restore refuses an occupied destination atomically. Permanent removal is a separate plan/approval against exact recorded staged identities; no unrelated holding item or Trash entry can be included.

M1A first proves the flow on disposable regular-file fixtures. It cannot expose arbitrary paths or recursive deletion. Production operation eligibility remains blocked until identity/content revalidation, persistent ownership, scope grants, interruption handling and recovery gates pass. Unknown content or receipt changes produce a visible blocker.

A same-volume holding destination must be available within the app's validated storage/access model. Cross-volume moves have no copy/delete fallback. Unsupported volume/access combinations remain review-only with a reason; they are not represented as successful cleanup. App-managed holding roots must be excluded from ordinary discovery and from descendant selections.

Evidence: probes/M0-EVIDENCE.md, probes/atomic-rename/README.md, probes/crash/README.md. The controlled APFS experiment separately demonstrated actual free-space recovery after deleting only its own staged fixture. It does not promise equivalent recovery on snapshot/shared/busy user volumes.

M0 remains open for its other required gates. M1A's review/storage contracts can progress independently of unavailable CI and unfinished Git/AI integrations; production execution stays blocked by the relevant unresolved gates. No milestone completion follows from this decision.
