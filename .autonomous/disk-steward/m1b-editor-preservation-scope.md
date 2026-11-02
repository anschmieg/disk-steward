# Grouped editor preservation coverage — 2026-11-02

Baseline PR20head d9a0040; branch feat/m1b-editor-preservation-tests. Preserve M1B's non-executable synthetic editor boundary: metadata/review edits never grant staging, purge, path or shell authority. M2 live discovery remains planned; no human-pilot or performance gate is passed by component tests.

Purpose: protect critical preservation interactions before the three-participant review pilot. Test blocked/Keep/group/duplicate/exclusion behavior in the real panel, and stale-revision/refresh/history/immutable-review orchestration in the workspace. Typed representative fixtures may be small for detailed interactions; the10,000-item test verifies bounded DOM and keyboard/focus behavior only, not native frame timing or accessibility certification.

Concrete UI bugs exposed may be fixed within those components while preserving narrow native command contracts and backend revision authority. No redesign, real user-file scan, new execution command, generic renderer access, cleanup eligibility, release or version promotion. Continue existing0.0.16 until a separate functional batch needs a patch bump. Human preservation/comprehension and native accessibility/performance validation remain explicit independent gates.

Finalbaseline: PR20 merged to mainb94f762. No concrete production defect was confirmed; only tests/fixture helpers/evidence are delivered. Workspace uses mocked IPC and panel only for orchestration; real-panel interactions have their own tests. The large flat navigation fixture does not replace the full participant-pilot scenario set.
