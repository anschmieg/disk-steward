# Implementation tasks

See the [roadmap](../../../ROADMAP.md) for deliverables/dependencies and the [MVP catalog](../../../MVP-CATALOG.md) for the named detector/authentication scope and verification budgets. M0 foundation work is in progress; see [progress](../../../PROGRESS.md). No complete M0 gate is claimed yet. This document authorizes no cleanup of user data.

M0–M6 remain the top-level milestones. M1A–M1C and M2A–M2B are sequential, demonstrable checkpoints inside M1/M2. The previous 12–16-week envelope is unvalidated: reforecast at M0 and M1A exits using observed effort and frozen scope. No calendar commitment is currently made.

## M0 — Foundation, feasibility, and scope freeze

- [ ] 0.1 Create minimal Tauri/React/TypeScript and shared Rust foundations; probe selected-folder access, denial/revocation, signing, sandbox variants, workers, native confirmation, system WebViews, and packaged-app testing. Compile portable crates on all three target OSes.
  - Requirements: R1.1–R1.4, R10.1–R10.2, R11.1–R11.5.
- [ ] 0.2 Prove one receipt-based holding/restore/selective permanent-removal mechanism on disposable fixtures, including identity/link/rename races, collision and crash reconciliation, and a controlled-volume space-recovery test.
  - Freeze the mechanism, UI terminology, allocation fixture, observation interval/tolerance, and unsupported volume behavior. Native Trash is not assumed to support every required operation without verification.
  - Requirements: R4.1–R4.7, R8.1–R8.2, R8.5.
- [ ] 0.3 Prove public and private GitHub HTTPS reads through the proposed transport/credential broker without source-clone changes, custom helpers, or secret leakage. Validate denied/revoked credentials and bounded temporary-store cleanup.
  - Implement resource-limit probes from MVP-CATALOG.md: network/disk/aggregate/time budgets, free-space reserve, cancellation overshoot, abandoned-store reconciliation. Public-only success does not satisfy this task.
  - Requirements: R5.4–R5.5, R5.8–R5.9, R8.3–R8.4.
- [ ] 0.4 Build the early AI evaluation harness and labeled development/held-out scenario split defined in design.md. Run the bounded development evaluation only with explicit credentials/consent; report routing, validity, usefulness, cost, latency, and baseline comparison.
  - Validate evidence packet adequacy before M1 contracts are frozen. Missing model access is an unresolved dependency, not a mock evaluation pass. Unrelated foundation tasks may continue.
  - Requirements: R7.1–R7.7, R11.8.
- [ ] 0.5 Validate and freeze the named detector catalog, supported tool/version matrix, executable subset, negative fixtures, Git authentication scope, and resource limits. Record feasibility decisions and a revised effort forecast.
  - Minimum: two executable generated-data/cache families plus exact duplicates for M3. A review-only downgrade is an explicit scope change, not completion of an executable detector.
  - Requirements: R2.1–R2.4, R5.9, R10.4.

Exit: feasibility evidence, supported operation/access model, measured resource constraints, early AI findings, named scope and target authentication are recorded. Unresolved gates and scope changes are explicit. Do not claim full M0 completion if required evidence is absent.

## M1 — Editable plan and complete removal lifecycle

### M1A — Smallest complete vertical slice

- [ ] 1.1 Define core inventory/evidence/action/revision/approval/journal types and SQLite migrations; build a five-action synthetic plan with keep/include, evidence, frozen review, native confirmation, and one supported staged-file operation.
  - Enforce typed commands, scope/identity checks, immutable manifest approval, edit invalidation, intent/outcome journaling and collision-safe restore. No AI or live discovery required.
  - Requirements: R2.1–R2.3, R3.1, R3.4, R3.6, R4.1–R4.5, R8.1–R8.2, R11.2–R11.3.

Checkpoint: demonstrate edit → approve → stage → results → restore; reject stale/unauthorized actions. Use measured effort to reforecast M1B–M6 before adding breadth.

### M1B — Full editor and review throughput

- [ ] 1.2 Add user-supplied items, remove-from-plan, descendant exclusions, duplicate-survivor choices, persistent exclusions, edit undo/redo, overlap/estimate rules, grouped decisions and readiness/blockers. Preserve user overrides and focus.
  - Requirements: R2.3, R3.1–R3.6, R6.3–R6.5.
- [ ] 1.3 Add the representative review fixture/protocol from design.md and instrument review time/interactions. Run the initial three-participant pilot and fix critical preservation/comprehension failures before progressing.
  - Separately test 10,000-action virtualization and keyboard focus. Record/calibrate usability targets transparently after M1A; do not use backend test success as a usability substitute.
  - Requirements: R6.4–R6.7, R3.1–R3.6.

Checkpoint: grouped editing is practical at realistic scale; every critical preservation task succeeds and blockers cannot be overridden accidentally.

### M1C — Permanent removal, interruption, and measurement

- [ ] 1.4 Implement selective permanent removal of exact recorded staged identities with a fresh second plan/approval; reconcile changed/missing/moved holding items; mark purged receipts non-restorable; never touch unrelated Trash or auto-purge.
  - Requirements: R4.1–R4.7, R8.1–R8.2, R8.5.
- [ ] 1.5 Test restore → stage again → separate permanent approval → measured free-space recovery on the controlled fixture. Cover cancellation/crash on both sides of staging, restore, and purge, including uncertain outcomes and no blind retries.
  - Requirements: R4.4, R4.6–R4.7, R2.2–R2.3, R8.5.

Exit: the complete editable/recoverable/permanent lifecycle works on disposable data; staged bytes are never presented as recovered bytes. All three checkpoints must pass.

## M2 — Live inventory and visual exploration

### M2A — Scanner and list-first explorer

- [ ] 2.1 Implement bounded/resumable metadata traversal, lossless identities, coverage/exclusions, cloud/library boundaries, pause/cancel, budgets, and invalidation. Connect real inventory to the editor and evidence inspector.
  - Test inaccessible scopes, odd names, hard links, symlinks/cycles, volume boundaries and cloud placeholders; no implicit hydration.
  - Requirements: R1.1–R1.5, R2.1–R2.4, R8.4, R11.5.
- [ ] 2.2 Build sortable/paged lists with subtle proportional bars behind row text, stable labeled denominators, exact sizes, linked selection, and keyboard/screen-reader support. Verify scan/resource budgets and honest occupied/candidate/selected/staged/recovered accounting.
  - Requirements: R2.2–R2.3, R6.2–R6.7.

Checkpoint: selected folders are usefully explored through the list even before the rich map is finished; coverage, cancellation and restart are reliable.

### M2B — Rich map and linked distributions

- [ ] 2.3 Implement the hierarchical map, explicit folder opening/breadcrumbs, category/type/size distributions, and decision-allocation visualization using bounded Rust aggregates and D3/SVG layouts. Preserve shared selection across views.
  - Resolve reported map defects: aggregate tiny cells, avoid clipped labels, validate hierarchy/area semantics, resizing, selection and light/dark contrast. Provide equivalent lists; do not treat the concept as accepted production geometry.
  - Requirements: R6.1–R6.6, R11.6–R11.7.
- [ ] 2.4 Repeat representative plan-review tasks against live/fixture inventory with linked visuals. Confirm that decision edits do not shrink occupied-space geometry or include blocked actions.
  - Requirements: R3.1–R3.6, R6.7, R11.6–R11.7.

Exit: both checkpoints pass; one-million-item scan and 10,000-action UI fixtures meet agreed budgets, incomplete coverage is explicit, and the review workflow remains understandable.

## M3 — Named offline detectors and actual space recovery

- [ ] 3.1 Implement the M0-frozen executable subset of Cargo outputs, CPython bytecode, pytest cache, and Homebrew cached downloads. Implement installer and large-item review from MVP-CATALOG.md without widening automatic eligibility.
  - Per-family tests: positive output, valuable/unknown additions, unknown version, active use, ownership uncertainty, changed-after-review and recovery. At least two generated-data/cache families must be executable.
  - Requirements: R2.1–R2.4, R3.2, R3.6, R10.4.
- [ ] 3.2 Implement exact duplicate grouping, streaming hashes and stable content verification, explicit survivors and link/overlap accounting; exclude managed-library internals.
  - Requirements: R2.3, R3.3, R4.2, R6.1.
- [ ] 3.3 Connect detectors to the supported stage/restore/permanent-removal adapters; repeat the complete lifecycle on manager/compiler-generated representative fixtures, not only hand-made filenames.
  - Demonstrate measurable controlled-volume recovery, retained valuable files and duplicate survivors; explain normal-volume discrepancies rather than promise a fixed delta.
  - Requirements: R4.1–R4.7, R8.1–R8.2, R8.5.

Exit: first useful internal offline alpha with at least two named executable generated/cache families plus duplicate cleanup, complete recovery/purge evidence, and a recorded supported-version matrix.

## M4 — Practical Git assessment with bounded verification

- [ ] 4.1 Implement safe Git discovery/inspection, full local-state inventory, activity ranking and size breakdown, including graphs/history completeness and explicit unsupported-layout states.
  - Requirements: R5.1–R5.3, R5.5, R5.7.
- [ ] 4.2 Implement the M0-validated public/private GitHub HTTPS broker and isolated verification stores, fresh remote/object proof, receipt expiry, source-state invariance, budget enforcement, cleanup and restart reconciliation.
  - Test low space before/during a fetch, disk/network/time/aggregate limits, cancel, forced worker termination, interrupted cleanup, cache reuse, revoked credentials and remote disappearance. Every incomplete proof blocks removal.
  - Requirements: R5.4–R5.6, R5.8–R5.9, R8.3–R8.4.
- [ ] 4.3 Build the repository inspector and ordinary verified-clone staging/permanent-removal proposals. Deduplicate parent clone/generated-child actions and apply all local-state dispositions explicitly.
  - Requirements: R3.3, R3.6, R4.6–R4.7, R5.1–R5.9, R8.1.
- [ ] 4.4 Validate disposable public and private repositories through the intended authentication UX, plus unpushed other branches, ignored secrets/data, stashes, detached/recovery commits, local tags/customization, complex layouts, malicious transports/configuration, stale refs and concurrent writes.
  - Unsupported SSH/Enterprise/other hosts must be explained; public-only success cannot pass this gate. No actual user repository is published or modified by acceptance testing.
  - Requirements: R4.2, R5.1–R5.9.

Exit: practical public/private acceptance cases pass, source clones are unchanged by verification, resource exhaustion cannot weaken proof, and unsupported/local-only cases remain visible and blocked.

## M5 — Production Jev and agent integration

- [ ] 5.1 Implement provider/credential adapters, consent/redaction, budgets/cancel, versioned profiles, response validation, cache invalidation and production Jev integration, carrying forward M0 findings.
  - Requirements: R7.1–R7.7, R8.3, R11.8.
- [ ] 5.2 Implement bounded reasoning tools, evidence-cited proposals, independent evidence-first review, and unresolved-case UI. Keep the executor inaccessible and test prompt injection, fake evidence, unsupported actions and provider failures.
  - Requirements: R7.3–R7.6, R4.3.
- [ ] 5.3 Evaluate the held-out M0 scenarios against the frozen criteria and deterministic baseline. Report routing, accepted-invalid-output count, usefulness, cost, latency and escalation; begin in shadow mode and only enable action proposals after the gate passes.
  - Do not substitute development scores or silently drop AI from the release after failed evaluation; record an explicit model/evidence/scope decision.
  - Requirements: R7.3–R7.7, R10.4.

Exit: held-out evaluation supports the chosen integration; no model output can create execution authority; the deterministic offline workflow still works during provider failure.

## M6 — Integrated macOS release

- [ ] 6.1 Complete authorization, IPC/XSS/CSP, crash/low-space, revocation, stale manifests/approval, restore collisions, selective purge and resource-budget integration suites. Disable any operation that cannot satisfy its gate.
  - Requirements: R1.3, R4.1–R4.7, R5.8–R5.9, R8.1–R8.5.
- [ ] 6.2 Run packaged-app tests, the final representative review-throughput pilot, native accessibility/keyboard tests, map/list checks and performance validation. Validate at least one consented representative pilot scope locally; never turn the pilot into unattended cleanup.
  - Requirements: R3.1–R3.6, R6.1–R6.7, R10.4.
- [ ] 6.3 Complete reproducible signed/notarized builds, locked dependencies, privacy-safe diagnostics, migrations and version checks. Publish the actual detector/tool/authentication support matrix and limitations; verify optional-tool/provider absence on a fresh Mac.
  - Requirements: R7.1, R8.3, R10.1–R10.4.

Exit: full stage/restore/separately approved purge/observed-recovery evidence exists for every enabled adapter; integrated safety, usability, access, signing and scope gates pass. M6 validates the integrated product; earlier milestones already include their own testing.

## Follow-on backlog (separate milestones)

- [ ] 7.0 Implement and validate Windows/Linux platform adapters and platform-specific cleanup catalogs, recovery, native confirmation, secure credential storage, packaging, accessibility, and destructive-operation suites. Enable actions per adapter only after target-OS tests pass; this is not included in the macOS-first estimate.
  - Requirements: R11.1–R11.5, R4.1–R4.6, R8.1–R8.4.

- [ ] 7.1 Implement reviewed commit/push preparation, local secret checks, destination review, hook/signing handling, and independent post-push verification that creates a new cleanup revision.
  - Requirements: R9.1–R9.2.
- [ ] 7.2 Implement conservative Git object maintenance, organizational branch cleanup, LFS pruning and graph-aware worktree adapters, each with dry-run/eligibility/recovery semantics and measured space changes.
  - Requirements: R9.3.
- [ ] 7.3 Implement verified Git bundle plus supplemental-state preservation and test restoration from its manifest before enabling archive-and-remove.
  - Requirements: R9.4, R8.1.
- [ ] 7.4 Implement near-duplicate image/document review and bounded archive-content comparison; keep personal-content removal explicitly selected with a retained survivor.
  - Requirements: R2.1–R2.4, R3.1–R3.6, R6.1–R6.5.
- [ ] 7.5 Implement supported application/package/container/VM cleanup adapters with dependency analysis, owner-native operations, and category-specific recovery tests.
  - Requirements: R2.1–R2.4, R4.1–R4.6, R8.1.
