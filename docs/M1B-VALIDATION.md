# M1B validation — first integrated editor slice

Version 0.0.5, feature branch `feat/m1b-plan-model`, based on M1A `cf939a4`. This is a non-executable synthetic editor, not a completed M1B or a user-file cleanup adapter.

The backend owns inventory projections, preservation choices, membership, descendant exclusions, duplicate survivors, revisions, bounded undo/redo and immutable reviews. SQLite persistence checks schema ownership, document sizes, digests and expected revisions. The native bridge accepts typed identities and one of two fixed example profiles, with no renderer-supplied paths or approval capabilities.

## Evidence

- All 113 workspace Rust tests passed, including 16 general-model and nine general-store tests. The transport tests used authorized local socket fixtures.
- Nine native backend tests and strict workspace/native Clippy passed during integration. Frontend type/build/version checks and formatting passed. The final macOS debug bundle built at 38.54 MiB.
- The native 1,000-item/25-group profile preserved a folder Keep edit, undo, redo and a saved review across app restart. The old review remained read-only after a later revision.
- The separate native 10,000-item/25-group profile loaded and supported bounded list/inspector rendering. End → Up → Return and Home → Down → Return selected the expected findings after a focus bug was fixed. Inspector exclusion controls retained keyboard focus after saves; toggling the same checkbox back restored the original selection count.
- Search filtered findings without changing whole-plan totals. Group bulk controls were disabled while filtering. A no-results view preserved the whole-plan summary. Exact proportional bars remain behind row text; their denominator is disclosed.
- Root review corrected group selected counts to use the same union of selected leaves as whole-plan totals, including descendants of grouped folders. A regression test covers ancestor/descendant overlap.

The 36 evaluation and 42 main-branch Python probe checks passed during this integration. GitHub CI did not run because of the existing account billing/spending restriction; local checks do not substitute for cross-platform CI.

## Open gates

Native-selected item additions, separately confirmed persistent global exclusions, and the three-participant preservation/comprehension pilot remain outstanding. M1B is **In progress**. Live recursive discovery belongs to M2. User-file cleanup, recovered-space claims, signed packaging and platform permission claims remain unavailable. No user files were scanned, staged or deleted in this validation.
