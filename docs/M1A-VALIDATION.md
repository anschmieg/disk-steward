# M1A validation checkpoint — 0.0.4

M1A is complete for its five-file disposable macOS debug slice. The complete disposable-file interaction has been observed in the native macOS debug app. This does not enable user-file cleanup or close the unresolved M0 gates.

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

After independent review fixes, all 88 workspace Rust tests and eight native backend tests passed. Workspace and native Clippy passed with warnings denied, formatting and frontend build/version checks passed. Git transport socket tests require local socket access; the restricted run failed with permission errors, and the authorized local-socket run passed. Those failures were not counted as successes.

The native debug bundle built at approximately 34.14 MiB. The final committed native smoke check confirmed the active navigation highlight, restored status, fresh staging eligibility and `0 bytes` recovered-space display. CI remains unrun because of the known GitHub account restriction. No signed/notarized, Windows, Linux or production permission claim follows from local macOS tests.

M0 still needs its independent human labels/development evaluation, complete authenticated Git worker/object proof, packaged permission matrix, catalog freeze and forecast. M1B review throughput and M1C permanent removal/actual-space recovery remain separate gates. The revised effort forecast is recorded in docs/IMPLEMENTATION-FORECAST.md. The independent safety review found expiry binding, interrupted-byte accounting and cross-process plan-edit gaps. All three are closed with regression tests; the final native smoke check confirmed owner edits (Revisions 5/6), an expiry-bound stage review and native cancellation with the restored file retained.


## Acceptance and scope of completion

| M1A gate | Evidence |
|---|---|
| Five-action editable plan, evidence and revision invalidation | Persistent plan store tests; native revisions and unchanged Keep decisions |
| Frozen review and exact native confirmation | Metadata snapshots remain non-executable; separate file review binds session, identities, contents, decisions, expiry and operation |
| Approval and intent before file operation | Core validation, one-time ledger, durable batch/item attempts; cancellation/replay/expiry/tampering tests |
| Supported recoverable stage, persistent results and collision-safe restore | Observed native stage → quit/reopen → restore; isolated fixture collision/content/link/concurrency tests |
| Interruption fails closed | Exclusive owner before recovery, uncertainty without retries, known completed totals retained, later files unattempted after uncertainty |
| Forecast before breadth | docs/IMPLEMENTATION-FORECAST.md, with remaining uncertainty and external waiting time separated |

The synthetic fixture snapshots are the inventory for this slice. General live inventory, variable plan membership, persistent exclusions, undo/redo and schema evolution for the general editor remain their named later checkpoints. No cleanup adapter for existing user files is enabled. M0's independent gates remain open and are not prerequisites silently waived for release.
