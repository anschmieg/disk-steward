# M1A validation checkpoint — 0.0.4

M1A is in validation. The complete disposable-file interaction has been observed in the native macOS debug app. This does not enable user-file cleanup or close the unresolved M0 gates.

## Observed desktop flow

On 2026-11-01 local time, the coordinator exercised the following flow through the actual Tauri interface:

1. Loaded the previously persisted Revision 4, with Test cache included and the other four actions kept; total selection 1 MiB.
2. Moved list selection and keyboard focus together with Arrow Down.
3. Saved the immutable review and opened its native preview. Its five decisions and exact byte total matched the backend snapshot. Cancel and Acknowledge both returned explicitly without approval.
4. Created the five registered disposable files, totaling 4.875 MiB. No existing files were adopted.
5. Prepared the selected 1 MiB stage review. Canceled its native dialog; all five files remained Available.
6. Restarted the rebuilt app, prepared a fresh review and confirmed staging of Test cache only. The result reported 1 MiB staged and zero recovered space.
7. Quit and reopened the app. The staged file, receipt, and completed result persisted.
8. Prepared a one-receipt restore review and confirmed it through the native dialog. Test cache became Restored, with zero recovered space. A fresh stage review was available afterward and was canceled, leaving the file restored.

The app was then closed. No permanent removal was attempted. The demo files remain in the app's registered private session; they are not user documents or a general cleanup scope.

## Contract checks

The reviewed service tests cover cancellation, single use before callback, expiration, backward clock movement, stale decisions and changed content during confirmation, restore collision, interrupted work without retry, live second-instance exclusion, uncertain ledger settlement, self-consistent manifest tampering, and stopping later items after an uncertain result. Fixture tests additionally cover journal-derived path constraints, bounded hashing, directory/file replacement, concurrent item claims, unknown schemas and no-follow database leaves.

The renderer cannot submit file paths, SQL, manifests, commands or approval booleans. The native bridge accepts typed references and rejects unknown fields or an identifier of the wrong type. The global native-operation guard also protects metadata edits throughout confirmation and execution. Backend state is revalidated after the native callback and before mutation; renderer disabled states are only presentation.

The control schema retains its original strict child-state vocabulary. For items deliberately skipped after uncertainty, the durable child state remains conservatively uncertain while the batch summary distinguishes `not_attempted`. No schema is silently reset to accommodate a development change.

## Evidence and limits

The workspace passed its then-current 83 Rust tests; the two subsequent service regression tests passed separately, bringing the set to 85. Seven native backend tests passed. Workspace and native Clippy passed with warnings denied, formatting and frontend build/version checks passed. Git transport socket tests require local socket access; the restricted run failed with permission errors, and the authorized local-socket run passed. Those failures were not counted as successes.

The native debug bundle built at approximately 34.14 MiB. Later cosmetic fixes distinguish the active navigation item and render zero as `0 bytes`; their frontend build passed, but that exact cosmetic revision still needs a final native smoke check. CI remains unrun because of the known GitHub account restriction. No signed/notarized, Windows, Linux or production permission claim follows from local macOS tests.

M0 still needs its independent human labels/development evaluation, complete authenticated Git worker/object proof, packaged permission matrix, catalog freeze and forecast. M1B review throughput and M1C permanent removal/actual-space recovery remain separate gates. The next checkpoint must record a revised effort forecast before M1A is marked complete or editor breadth is added.
