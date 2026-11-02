# M1B purpose and first implementation boundary

M1A completed the exact review/confirmation/recovery chain on five app-created files. M1B makes reviewing and editing a realistic plan practical: membership, descendant exclusions, duplicate survivors, user overrides, undo/redo, grouping, estimates and blockers. The product's purpose remains understanding storage and choosing exact reviewed changes. More findings are useful only if preservation choices remain understandable and authoritative.

The first batch builds a shared general editor domain model with isolated synthetic inventory. It is separate from the fixed five-file M1A demo and cannot authorize that demo's operations or any user-file action. Every general-editor projection/review remains explicitly non-executable. Later batches add persistence/migrations, typed native commands, the grouped virtualized interface, and native-selected metadata additions. Live recursive scanning remains M2; production cleanup eligibility remains adapter-specific and unavailable here.

The model separates immutable inventory/evidence and suggestions from user choices. Refresh must preserve overrides by identity. Removing an item from a plan never deletes inventory or a file. Descendant exclusions are explicit identity references, with no renderer-supplied paths. Duplicate survivor changes protect the selected survivor and invalidate affected consequences. Group Include never overrides a blocker. Unknown identities, malformed trees/groups, cycles, ambiguous overlap or storage sharing must not inflate estimates or make an action ready.

Every meaningful edit, including undo/redo, creates a new revision; old reviews cannot regain authority. No-op edits preserve revision. Undo/redo is bounded and a new edit after undo discards redo. The domain bounds inventory size, labels, history and serialized state; a 10,000-item fixture must not make focus or memory unbounded. Logical selection, a conservative recovery estimate and actual recovered bytes remain different quantities; no staging estimate is presented as freed space.

Completion still requires all M1B gates: native item addition/removal/exclusions, survivor and override handling, persistent exclusions with separate explicit confirmation, undo/redo, overlap/estimate rules, readiness/blockers, a 1,000-finding/25-group review fixture, 10,000-item focus/performance verification and the three-participant preservation/comprehension pilot. Domain or automated tests alone cannot close that pilot. M0 remains open in parallel; its unsigned access and AI/Git limitations are not relaxed.

Work is on feat/m1b-plan-model, version0.0.5. The M0 follow-up is isolated at /Users/adrian/Projects/disk-steward-worktrees/m0-feasibility on feat/m0-feasibility-followup, version0.0.4. Matching versions are enforced within each branch, including first-party lockfile entries. No release is published.


## Native metadata-add boundary

The next integrated batch permits one native-selected regular file in the pilot workspace. It observes leaf metadata only, persists a bounded display basename/size with fresh opaque identities, defaults to Keep and attaches a hard `native_metadata_only` blocker. No source path, content, permission grant or operation binding is retained. Folder traversal, duplicate identity matching, source revalidation and eligibility remain later discovery work. Repeated selection may create separate blocked observations; it is not claimed to deduplicate source identities. Addition uses the existing model state/history shape and trusted append transaction, preserving saved reviews and undo/redo without a v1 schema change.

Native selection is not broad inventory completion or M1B closure. Persistent global exclusions need their own separately confirmed registry and stable source semantics; three human pilots remain outstanding.
