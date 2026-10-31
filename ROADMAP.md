# Disk Steward implementation roadmap

Status: **M0 In progress; M1A In progress (review/storage slice only)**; M1B–M6 and post-MVP tracks remain Planned. The first foundation slice is being implemented on feat/m0-foundation; see [PROGRESS.md](PROGRESS.md). Concepts and the current desktop scaffold use synthetic data. This roadmap authorizes no cleanup, repository changes, remote access, or model requests.

## Delivery approach and forecasting

Build one Tauri application with a shared Rust core, supported macOS cleanup first, and validated Windows/Linux adapters later. Keep the editable plan and exact user approval central from the first complete slice.

The earlier 12–16-week figure is an unvalidated placeholder, not an active delivery forecast. Reforecast after M0 using the frozen scope and feasibility results, then again after M1A using measured implementation effort. The roadmap expresses dependency order and observable outcomes rather than premature calendar dates. Required validation is not compressed to fit a week number.

Read alongside the [detailed tasks](.kiro/specs/macos-cleanup/tasks.md), [architecture](.kiro/specs/macos-cleanup/design.md), [requirements](.kiro/specs/macos-cleanup/requirements.md), and [named MVP catalog](MVP-CATALOG.md).

## MVP milestones

| Milestone | Demonstrable outcome | Dependency / completion gate |
|---|---|---|
| **M0 — Feasibility and scope freeze** | Working foundation/probes, early AI evaluation, named detector scope, practical private Git authentication, and bounded verification | No predecessor. Record validated operations, authentication, resource limits, evidence format and revised forecast; unresolved gates remain visible |
| **M1A — Smallest complete plan slice** | Five-action plan: edit → review → approve → recoverable stage → results → restore | M0 contracts. Backend rejects stale/unauthorized actions; reforecast remaining work using actual effort |
| **M1B — Full editor and usable review** | Add/remove items, exclusions, survivors, overrides, undo/redo, grouped decisions and blockers | M1A. Representative review pilot passes preservation/comprehension criteria |
| **M1C — Complete removal lifecycle** | Restore → stage again → separately approve permanent removal → observe recovered space | M1A, integrated with M1B. Receipt-based selective purge, interruption/reconciliation and controlled-volume measurement pass |
| **M2A — Live scan and list explorer** | Resumable scanning, honest coverage, evidence and sortable list with background size bars | M1. Real selected scopes are useful without waiting for the map; resource/access/cancel tests pass |
| **M2B — Rich linked visualization** | Hierarchical map, breadcrumbs, file/type/size distributions and decision allocation | M2A. Map defects are resolved, all views agree, accessible list remains available and review remains usable |
| **M3 — Useful offline cleanup alpha** | Named detectors produce eligible plans that actually recover space after review | M1–M2. At least two generated/cache families plus exact duplicates pass full lifecycle tests |
| **M4 — Practical Git assessment** | Ordinary public/private verified clones can be proposed for removal | M3; M0 transport decision. Private authentication, full local-state checks, bounded verification and adversarial cases pass |
| **M5 — Production Jev and agent investigations** | Opt-in bounded investigation and evidence-backed reviewed proposals | M1 contracts and M0 evaluation; final integration after M4. Held-out evaluation and provider-failure gates pass |
| **M6 — macOS MVP release** | Signed/notarized, usable application with supported operations and explicit limits | All preceding gates; integrated recovery, actual-space, usability, accessibility, permissions and release tests pass |

M1A–M1C are checkpoints within M1, and M2A–M2B within M2. They are separately demonstrable without changing top-level task IDs. Provider scaffolding and portable-core tests can overlap other work where capacity permits; no milestone is declared complete because an unrelated stream passed.

## M0 decisions that must be recorded

1. **Holding/recovery mechanism:** validate native Trash with exact receipt-based restore/selective purge, or choose a clearly labeled app-managed recoverable holding area. Never call an internal directory Trash. Neither mechanism frees space until a separately approved purge.
2. **Executable detector subset:** freeze tool versions, roots, provenance checks, exclusions and fixtures from MVP-CATALOG.md. At least two of Cargo output, CPython bytecode, pytest cache, and Homebrew downloads must support execution by M3, alongside exact duplicate cleanup. Review-only categories are labeled, not counted as executable successes.
3. **Practical Git authentication:** public and private GitHub HTTPS with app-mediated credentials held in Keychain. Revoked/denied credentials must fail correctly. SSH/other-host support is explicitly outside this initial target, with no silent source-remote rewriting. Public-only tests cannot pass the planned Git gate.
4. **Verification resource limits:** starting defaults of one job, 2 GiB network/temp storage per repository, 4 GiB aggregate stores, a 2 GiB free-space reserve plus checkpoint headroom, and five minutes per attempt. Calibrate and prove enforcement/cleanup, including cancellation overshoot. Budget failure leaves verification incomplete and removal blocked.
5. **Early AI feasibility:** label the development/held-out scenarios and run a bounded, consented development evaluation. Validate routing, investigation usefulness, evidence adequacy, cost and latency before finalizing M1 contracts. Missing credentials are an explicit dependency, not an evaluation pass.
6. **Forecast:** use the results above to estimate the actual frozen scope. A scope reduction needs an explicit recorded decision; do not quietly remove a requirement to close M0.

## Complete removal is part of the MVP

The required lifecycle is:

`review → approve recoverable stage → inspect/restore if desired → separately review exact staged items → approve permanent removal → verify outcome`

There is no automatic purge at retention expiry and no inclusion of unrelated Trash. Staged identities are checked again; changed, missing, renamed or moved items require reconciliation. Permanent removal ends restoration availability for those receipts.

M1C proves the lifecycle on disposable fixtures. M3 repeats it with representative detector outputs and duplicate groups. Controlled tests use fully allocated non-sparse/non-cloned fixtures on an isolated volume, with explicit measurement tolerances. Normal user-volume results remain honest about snapshots, sharing and other writers; the app never deletes snapshots to make an estimate come true.

## Review usability is a completion gate

M1B, M2B and M6 run the protocol in design.md: 1,000 findings in 25 groups, including blockers, mixed folders and duplicates. Three representative participants perform the prescribed keep/exclude/add/remove/survivor/comprehension tasks. Initial target: each completes within 15 minutes, every critical preservation decision is correct, and no moderator intervention is needed to avoid unintended removal. Freeze/calibrate targets transparently after M1A; do not average away a failed preservation decision.

A separate 10,000-action fixture tests virtualization/focus/performance. Humans should review meaningful groups and exceptions, not approve every leaf file individually. Accessibility is checked separately.

## Release checkpoints

- **After M1A:** first complete product interaction; use measured effort to reforecast.
- **After M2A:** useful read-only internal explorer.
- **After M3:** useful offline cleanup alpha with real space recovery.
- **After M4:** Git-enabled internal alpha meeting the named private/public authentication target.
- **After M5:** AI-integrated beta candidate validated against held-out scenarios; deterministic operation still works offline.
- **After M6:** first supported macOS public release. A failed action adapter remains disabled; any removal of promised scope is explicitly recorded and communicated.

Testing accompanies every milestone; M6 validates the integrated release rather than starting safety/usability work for the first time.

## Review findings mapped to changes

| Finding | Revision and acceptance evidence |
|---|---|
| Trash alone does not recover space | R4.6–R4.7 and R8.5; M1C proves selective purge/measurement, M3 repeats with detector outputs |
| Verification can consume the disk being cleaned | R5.8; M0 transport/storage probes and M4 budget/cancel/crash suites; limits in MVP-CATALOG.md |
| AI value is discovered too late | R7.7; M0 development evaluation before evidence contracts, M5 held-out evaluation |
| Initial detector scope is vague | R10.4; named D1–D8 catalog and M0-frozen executable minimum, checked at M3 |
| Public-only Git tests may hide product limitations | R5.9; public/private GitHub HTTPS acceptance, denied/revoked credentials and explicit unsupported transports |
| Oversized milestones and untested review effort | M1A–M1C and M2A–M2B; R6.7 review pilot plus measured-effort forecasts at M0/M1A |

## Post-MVP tracks

| Track | Deliverable / gate | Existing task mapping |
|---|---|---|
| **P1 — Preserve Git work** | Reviewed commit/push and verified bundle/archive preservation; successful publication creates a new removal proposal, never automatic deletion | 7.1, 7.3 |
| **P2 — Maintain repositories that stay** | Measured Git housekeeping, explicit branch review, LFS and worktree adapters with retention/concurrency/recovery tests | 7.2 |
| **P3 — Similar content** | Near-duplicate images, document revisions and archive overlap; comparison-first review with explicit retained material | 7.4 |
| **P4 — Broader integrations** | Application/CLI uninstall and container/VM cleanup using ownership/dependency evidence and category-specific recovery | 7.5 |
| **P5 — Windows/Linux releases** | Separate OS gates for native operations, platform profiles/catalogs, permissions, allocation, identity, recovery, packaging and accessibility | 7.0; split by OS when scheduled |

These are independent prioritizable tracks, outside the initial macOS forecast. Shared plan/evidence/approval/recovery contracts remain mandatory. Additional Git transports and hosts are separate integrations, not implicit additions to M4.

## Tracking

Track Planned → In progress → Validation → Complete. Use Blocked for a concrete unresolved dependency. Each checkpoint closes with a demo/build, verification results, supported-scope matrix, known limitations, measured effort and forecast impact. Implementation is authorized and M0 is in progress; no milestone gate is complete merely because a scaffold exists.
