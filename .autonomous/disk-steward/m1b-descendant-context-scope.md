# M1B descendant preservation context

The folder inspector's Keep items inside checklist must distinguish same-named descendants under different nested folders. Show relative ancestor labels from existing inventory parentId links as visible secondary text and part of the checkbox's accessible name. Actions continue sending exact IDs; no source path is discovered or retained, no IPC/schema/eligibility change.

Calculate context only for the visible 50-row page, using the existing item map, visited IDs and at most 64 ancestor steps. Keep the existing descendant traversal depth and search semantics. Missing/cyclic context is bounded and labeled incomplete; no UI claim of comprehensive inventory. Wrap long context within the inspector. Regression uses two notes.txt entries under distinct folders and proves one selection sends only its exact ID.

Independent main-based UI fix. 37 frontend tests, TypeScript/production build, version0.0.17 check, formatting and diff checks pass locally. DOM accessible labels are tested, not native screen-reader/focus/preservation/comprehension pilot acceptance. No user-file operations or cleanup authority, release or milestone completion. M1B human/native gates remain open. Transport work remains parked at PR82; progress-merge maintenance PR83 remains pending.
