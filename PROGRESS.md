# Implementation progress

## M0 — In progress (foundation slice, version 0.0.1)

Purpose: help users understand storage and approve exact, editable cleanup decisions before changes. M0 establishes whether access, evidence, resource limits, and recovery can support that promise.

Scope cornerstones: typed backend boundaries; portable core with native adapters; honest unavailable/unsupported states; no real filesystem cleanup in the scaffold; versions remain 0.0.x until authorized otherwise.

Implemented with GPT-6 Luna subagents and coordinator review:

- Tauri 2 / React / TypeScript scaffold with a synthetic storage review UI, coherent 96.3 GiB fixture totals, illustrative map labeling, and proportional list background bars. No custom filesystem, shell or execution commands.
- Portable Rust contracts for opaque IDs, scope state, action bindings and revision-bound approval validation. These are not yet connected to the desktop backend and are not a replay-safe executor; durable single-use consumption remains future work.
- 100 provisional Luna-authored evaluation cases with 60 development / 40 held-out scenario-family separation, conservative baseline and malformed-response/evidence/action rejection tests. Human label review and live model evaluation remain outstanding; rule-matched baseline accuracy is not AI evidence.
- Feature branch feat/m0-foundation, private GitHub repository, CI for shared-core checks on macOS/Windows/Linux, frontend checks, and independent macOS desktop compilation. Version guard requires all app versions to match 0.0.x.

Verified locally on native aarch64-apple-darwin:

- npm production build / TypeScript / version guard passed.
- npm audit reported zero vulnerabilities at check time.
- cargo test --workspace --locked: five tests passed; formatting and strict Clippy passed.
- cargo check --manifest-path src-tauri/Cargo.toml --locked -j 2 and native formatting passed. This is compilation, not a packaged runtime/permissions test.
- Five Python validator tests and fixture validation passed; held-out labels were not scored.
- Browser smoke: fixture total, list bars, selection-to-example-plan, and disabled execution verified.

CI infrastructure blocker: GitHub run 41240218301 did not start its jobs because GitHub reports failed account payments or an insufficient spending limit. No code checks ran there; cross-platform compilation remains unverified. Account billing was not changed. Draft PR: https://github.com/anschmieg/disk-steward/pull/1 . Rerun CI after the account restriction is resolved.

Outstanding M0 gates: permission/recovery experiments, private Git authentication and bounded verification probes, independent human label review plus consented live AI development evaluation, supported detector/version freeze, native runtime/packaged-app testing, desktop/core integration, and revised forecast. A scaffold does not complete M0. No actual user files were scanned, moved, or deleted by this implementation.

## Autonomous continuation — 31 October 2026

Purpose/scope review: M0 must establish truthful platform support and recovery evidence before user-visible cleanup can be enabled. This batch keeps every production adapter unsupported and runs destructive probe operations only on internally created disposable fixtures.

- Luna added shared platform/capability reports that distinguish unavailable from unsupported; reported availability never grants mutation authority.
- The desktop now depends on the shared core and stores backend-owned capability reports at startup. Every unimplemented production capability reports unsupported. No new renderer commands or live filesystem access were added. This supersedes the earlier unwired-core note; frontend IPC and validated platform adapters remain future work.
- Luna added a development-only temporary-directory stage/restore/purge probe. Coordinator review closed a name-traversal gap and added registered-fixture enforcement, exclusive creation, and inode/device/content fingerprints. Five tests cover collision, changed contents, selective purge, traversal/unregistered names and interrupted-move reporting. The probe does not prove native Trash, atomicity, crash durability, or production adapter safety.
- Native desktop cargo check with the core linked passed; core tests now total seven, and formatting/strict Clippy pass. Python probe tests total five. Version guard remains 0.0.1. CI includes the new probe tests but is still externally blocked until the GitHub account restriction is resolved.

M0 remains in progress. Next: native permission/runtime probes and recovery-mechanism feasibility; independently reviewed evaluation labels/live model evaluation; private Git broker/resource limits; detector scope/version freeze and forecast. No milestone completion claimed. Quota checks during this batch reported 46% remaining.

## Sustained M0 feasibility — version 0.0.2

The resumed work uses the autonomous skill's durable handoff pattern with Luna delegates; the app heartbeat is the sole continuation mechanism. Routine commits and completed test batches are checkpoints, not stopping conditions.

- Added a permission-scoped read-only native status command. Built and launched the debug macOS bundle; its native interface reported 0/7 production adapters available and list navigation worked. The UI remains synthetic with execution disabled.
- Native Foundation fixtures passed same-volume stage/restore, collisions, symlink rejection, same-size edit detection, exact returned-URL Trash restoration, and optional selective removal while a separate synthetic sentinel survived. Restricted execution failed with Cocoa 513; access-enabled fixture runs succeeded.
- An isolated 4 GiB APFS image recovered 276,832,256 bytes after exact fixture deletion, against 276,824,064 allocated bytes. Stage/restore did not free space. The probe detached and removed its own image. No user snapshots or files were touched.
- Synthetic Keychain create/read/delete/absence passed. Ordinary bookmarks passed identity/persistence checks, but read-only security-scoped bookmark creation failed in the unsigned CLI; scoped access remains unproven.
- Public and authenticated-private GitHub HTTPS advertisements passed bounded read-only probes; anonymous private access failed. No clone/object completeness proof or production credential broker exists yet. Added pure verification-budget accounting with seven behavioral tests; failed/canceled workers retain accounting until stopped acknowledgement.
- Four development-only Jev requests succeeded on typesafe/jev-1.14-20261014, total reported cost $0.000087444. Provisional labels are still not human-reviewed. One initial route differed from its provisional label; these calls establish connectivity only.
- CPython 3.15.0 checked-hash bytecode regenerated exactly in fixtures; negative cases were rejected. pytest 9.2.0 generated a recognized nodeids record in a disposable environment, subsequently removed. These remain candidate version pins, not production detector support.

Detailed observations and limitations: [M0 evidence](probes/M0-EVIDENCE.md). M0 remains in progress. Durable approval-ledger feasibility is under review. Remaining work includes packaged permission/confirmation boundaries, real broker/worker enforcement, human evaluation and detector scope freeze, and an evidence-based forecast. CI remains blocked by the previously reported GitHub account restriction.

Verification checkpoint: 20 workspace Rust tests, strict Clippy and formatting pass; native backend check and 0.0.2 debug app bundle build pass; 26 evaluation and 25 probe Python tests pass. SQLite now prevents both repeated consumption and multiple intents for a consumed approval; unknown/altered schemas are refused. A schema-validation query error and an associated test barrier hang were found and corrected before integration. The ledger remains a feasibility component without filesystem authority. Native helper compilation/dry-run checks were added to CI, but those jobs have not run because of the account restriction.

## M0 desktop access and worker follow-through — 0.0.3

Purpose review: a standalone desktop utility needs truthful native access/confirmation behavior, not just successful command-line probes. Added official Tauri dialog integration behind two input-free custom permissions; generic dialog/filesystem calls remain unavailable to the renderer. Backend serialization allows only one native check at a time. Folder check opens a chooser, rejects non-directories/links, opens a read iterator without consuming entry names, and retains no scope/path. The native review preview contains a fixed backend-owned example and cannot issue an approval or execute anything.

Observed in the 0.0.3 app: the native folder chooser opened; selecting a newly created empty fixture reported readable; canceling a second check reported canceled/no retained access. Fixture directory was removed afterward. Restricted process launch aborted; access-enabled process launch worked. This is access-boundary evidence, not signed-distribution or security-scoped persistence proof.

The disposable worker harness verifies parent-issued stopped receipts, registered store identities, forged-receipt/path/symlink rejection and stop/join before cleanup. Coordinator run: 262,144 allocated bytes against 65,536 budget (196,608 overshoot); deadline case 0.153 seconds against 0.150 seconds. Both workers stopped and their stores were removed. The overshoot makes production headroom and wire-level limits mandatory. Added one synthetic-invalid credential request to the existing private app repository; HTTP 401 correctly blocked access.

Transport selection remains provisional: gix-first investigation is documented, but neither candidate has yet proved complete ingress/disk/time/free-space enforcement. See probes/https/TRANSPORT-DECISION.md and probes/M0-SUPPORT-MATRIX.md. No M0 completion claimed.

Native review correction: the first unparented dialog was not visible in the app tree and an Escape input resulted in acknowledgement; it had no authority and changed nothing. Explicitly parenting the dialog to the main Tauri window produced a visible native sheet with backend-owned summary. Clicking Cancel and pressing Escape in separate runs both returned “Review preview canceled. Nothing changed.” This is dialog feasibility only, not a revision/digest-bound production approval. Folder-check wording now says no folder is saved, avoiding any claim that an OS permission is revoked. The chooser disables directory creation.

0.0.3 checkpoint validation: 20 portable Rust tests and one native backend unit test passed; strict workspace/desktop Clippy, formatting, frontend build and expanded version guard passed. Python suites passed26 evaluation/34 probe tests. A Luna adversarial review found stale “no filesystem access” copy in the plan view; it now accurately describes the optional read-only folder check. CI includes the native unit test but has not run externally.

### Additional M0 integrity checks

- Native folder access tests now reject missing/replaced files, leaf symlinks and revoked POSIX read permission, using disposable directories. All three native backend tests pass; this is not the macOS TCC denial/revocation matrix.
- Core approval construction now refuses duplicate action/scope identities and missing, conflicting or blank adapter version bindings. These contracts still grant no filesystem authority.
- Real subprocess interruption before rename and after rename/before completion journal reconciled to original/staged; neither recovery retried the move. Parent-directory identities and bounded strict-schema control records are checked. Changed content/collision/tampered receipt/parent replacement block restore. Native Trash and power-loss remain separate unproven cases.
- Added a blind terminal-only manual development-label review helper. Resume binds reviewed facts/evidence/route definitions with SHA-256 and rejects malformed, stale, duplicate or held-out records. Coordinator corrected bounded line reading and invalid UTF-8 handling. No human review records were produced.
- Compiled gix HTTP/body accounting with aggregate atomic reservation; review caught a two-body counter race, now covered by a deterministic barrier test. Response-body exposure is not network wire accounting. A real loopback socket fixture and safe native no-overwrite rename API are the next bounded feasibility experiments.

Latest coordinator Python checks:36 evaluation tests and42 probe tests passed. Version remains0.0.3. M0 remains open.

M0 consolidation:45 workspace Rust tests pass, including15 Git transport/loopback tests and6 native atomic-rename tests; three native backend tests pass. Workspace strict Clippy/formatting, frontend build/version guard,36 evaluation tests and42 probe tests pass. Local socket fixtures needed execution access; restricted binding failures were not counted as passing. Fixture servers now have finite accept/read/write lifetimes, and slow-drip sends repeated sub-timeout bytes to test the overall deadline.

Typed IDs now persist as strict versioned/type-specific canonical UUIDv4 references from OS randomness; full Approval capabilities remain nonserializable. Chosen first lifecycle mechanism is app-managed same-volume recoverable holding, documented in docs/decisions/0001-recoverable-holding.md. No production adapter is enabled by this decision.

M0 remains In progress: runnable cross-platform CI, independent human labels/development evaluation, full private HTTPS credential/worker/object proof, packaged access matrix, detector freeze and forecast remain open. M1A review/storage work may proceed from tested M0 contracts while these gates remain explicit; it must not be represented as a completed lifecycle or allow user-file execution.

## M1A — In progress, review-only slice (0.0.4)

Purpose: make the editable cleanup plan authoritative and persistent before adding execution or discovery. M0 remains open; M1A uses its tested contracts while external and unrelated gates remain explicit. The full M1A lifecycle is not complete. Scope/failure review is recorded in .autonomous/disk-steward/m1a-scope.md.

Luna implemented the SQLite example-plan service and the linked editor/evidence/review interface. Coordinator added explicit Tauri permissions/typed requests, bounded blocking database work, sanitized failures, schema/leaf-link checks and native verification. The five-action dataset totals 4.875 MiB and is explicitly synthetic/non-executable. Keep/Include edits use expected-revision tokens; frozen reviews survive later edits and reopening.

Observed in native 0.0.4 debug app: initial Revision 1/all Keep; Include 3 MiB + 1 MiB yielded Revision 3/4 MiB; saved review displayed all five decisions. After quitting/relaunching, Revision 3, both decisions and identical review fingerprint were preserved. A later Keep edit yielded Revision 4/1 MiB and a distinct review. No user files were scanned or changed; writes were only the app's synthetic plan database.

The editor uses exact proportional row backgrounds with one labeled denominator, linked evidence, pending/error states, keyboard focus navigation and pressed decision states. Browser mode is an explicitly unsaved preview. Uncertain native saves require refresh before further edits/review. Snapshot digests are optional details. No approval or staging command is exposed.

Review found and closed two database ownership gaps: views/triggers count as existing schema objects, and SQLite refuses a symlinked database leaf. Trusted parent aliases must be resolved separately so macOS /var works without following the leaf. Tests cover both plan and approval stores. A separate fixture-lifecycle component is still isolated pending coordinator review; journal-derived paths and cross-instance operation claims must pass regression checks before integration.

Review-only checkpoint validation:57 workspace Rust tests passed, including10 plan-store and8 approval-ledger tests; strict workspace/native Clippy and formatting passed; frontend build/version guard passed;36 evaluation and42 probe Python tests passed. Native0.0.4 debug bundle built and persistence flow was exercised. Later keyboard/error-state refinements compile but still need a final native interaction check. CI remains unrun due the known account restriction. No milestone completion or release is claimed.


## M1A — Native disposable lifecycle in validation (0.0.4)

The persistent plan now connects to a separate app-created fixture workflow: create five files → prepare exact review → native confirmation → one-time approval → recoverable stage → persistent results → fresh review/confirmation → restore. Existing metadata reviews remain non-executable. The coordinator observed cancellation, stage, quit/reopen, restore and preparation for restaging in the actual desktop app; details and limitations are in docs/M1A-VALIDATION.md.

Luna implemented the bounded execution service, fixture component, lifecycle UI and native bridge. Coordinator review closed live-instance recovery, journal-derived paths, unbounded hashing, schema ownership, post-claim uncertainty, selected-action tampering, and edit-versus-execution races. An exclusive OS lease precedes recovery. Ambiguous outcomes retain files and block further preparation; no automatic retries or permanent removal exist. Staging and restoration report zero recovered space.

Validation:83 then-current workspace tests passed with local socket access; two later service regression tests passed separately (85 total in the current set). Seven native backend tests passed; strict workspace/native Clippy, formatting, frontend build/version policy and actual macOS debug bundling passed. Final cosmetic navigation/zero-unit edits compile and await one final native smoke check. Cross-platform CI is still unrun due the known account restriction. M1A remains Validation pending final checkpoint/forecast; M0 remains In progress with its existing external and technical gates. Draft PR2 remains stacked on PR1. No release was published.


## M1A — Complete within the disposable slice

Independent review closed three concrete gaps: the exact prepared-review ID/creation/expiry are now hashed and lifetime-checked; crash recovery retains completed child-byte totals; and a second live app instance cannot edit the owner's plan. All88 workspace Rust tests and8 native tests passed, strict workspace/native Clippy and formatting passed, and the final0.0.4 bundle built. Native smoke check confirmed owner edits to Revisions5/6, new expiry-bound review and native cancel with Test cache still Restored. The app is closed. docs/M1A-VALIDATION.md records the acceptance matrix; docs/IMPLEMENTATION-FORECAST.md replaces the old calendar placeholder with42–75 provisional verified work batches across the remaining scope, including external waiting-time limits.

M1A's five-action disposable-file lifecycle is complete; general inventory/editor breadth and user-file execution are not included in that claim. M0 remains In progress. M1B is next, following a new purpose/scope/failure checkpoint. Versions remain0.0.4; no release is published. M0 cache and HTTPS worker probes are progressing independently and will be kept in a separate feature branch/PR.


## M1B — In progress, bounded domain batch (0.0.5)

Purpose/scope/failure checkpoint: .autonomous/disk-steward/m1b-scope.md. The first batch generalizes plan editing over a validated synthetic inventory, preserving user overrides separately from suggestions and adding membership/exclusions/survivors/undo/redo/groups/blockers with honest overlap estimates. It remains non-executable and separate from the five-file demo. Persistence, native additions, interface integration and participant pilots remain later M1B batches. A domain implementation is not milestone completion.

Current feature branch is feat/m1b-plan-model based on completedM1A cf939a4. Versions are0.0.5; the version guard now also checks first-party Cargo lock entries. M0 follow-up code/evidence is isolated in /Users/adrian/Projects/disk-steward-worktrees/m0-feasibility on feat/m0-feasibility-followup at0.0.4, ready for a separate reviewable PR. No release is published.


## M1B — Integrated persistent editor slice (0.0.5)

The general editor now runs in the native app with separate 1,000- and 10,000-item synthetic workspaces, persistent revisions/undo/redo, membership, descendant exclusions, duplicate survivors, group choices, blockers and immutable saved reviews. The list has proportional background bars, bounded rendering, search and a linked evidence inspector. Root review corrected overlapping group counts and a native keyboard-focus failure. All 113 workspace Rust tests, nine native tests, strict workspace/native Clippy, formatting and frontend build/version checks passed during integration; the final macOS debug bundle built at 38.54 MiB. Native remove/re-add preserved the prior choice and returned the pilot selection to 790 at Revision 6. Saved-review persistence and 10,000-item keyboard navigation were observed in the app. See docs/M1B-VALIDATION.md.

M1B remains In progress: native-selected additions, separately confirmed persistent global exclusions and the three-person preservation/comprehension pilot remain open. M0 stays In progress, CI remains unavailable due the known account restriction, and no user-file cleanup or release is enabled.


M1B metadata-add follow-up: native file selection adds a blocked basename/size observation with default Keep; no path/content/grant/operation identity is retained. All117 workspace/14native tests and strictClippy/build checks pass. Native cancel/add/undo/restart/redo passed on an app-created47-byte fixture (current pilotRev9/1,001entries/790selected). Independent review found directional filename formatting could mislead display; these characters now render as visible escapes, with regression coverage. Persistent global exclusions, directory discovery/eligibility and three human pilots remain open. PR4 remains a partial M1B checkpoint at0.0.5.


## M1B — Remembered exclusions in progress (0.0.6)

Editor PR4 checkpoint b3cd202 is pushed. New feature branch feat/m1b-exclusions implements a separate exact-directory registry, native identity observations and separately confirmed add/remove settings. Its rules are explicitly for future scans, not applied to the synthetic plan or blocked source-less metadata entries. Scope/failure review is recorded in .autonomous/disk-steward/m1b-exclusions-scope.md. Storage/native/UI code is under review; no acceptance or milestone completion is claimed. Matching versions are0.0.6.


The0.0.6 exclusion registry slice is verified:128workspace/24native tests, strictClippy/fmt/frontendbuild/version and40.07MiB native bundle pass. Native cancel-add/confirm-add/restart/cancel-remove/confirm-remove passed on a disposable directory, preserving the existing plan revision and fixture sentinel. Private directory/database side-file modes were0700/0600. No source paths or identity bytes reach the renderer, and rules remain saved-unverified for future scans. Details are in docs/M1B-VALIDATION.md. M1B remains In progress; folder additions/eligibility, M2 rule application and human pilots remain open.


M1B0.0.7 adds blocked selected-folder metadata through a native picker, with unknown size and explicitly un-inventoried contents. Native26tests/strictClippy/fmt/frontendbuild/version and40.14MiB bundle pass. Native cancel/add/restart/undo/redo passed on the disposable directory: current pilotRev12/1,002entries/790selected,0remembered exclusions, sentinel unchanged. Full source discovery/eligibility and human pilots remain open; M1B is not complete. Work is on feat/m1b-folder-metadata, stacked on PR5.


## M1C — In progress, bounded migration preparation (0.0.8)

PR6 folder metadata checkpoint4e49476 is pushed. Work starts on feat/m1c-purge-schema, matching0.0.8 versions. Scope and failure cases are recorded in .autonomous/disk-steward/m1c-scope.md. The first experiment uses disposable lifecycle databases to prove allowlisted, rollback-safe v1→v2 migration before a purge operation is integrated. It exposes no deletion command and does not migrate the running app. M1B human pilots remain open; M1C lifecycle/native/permanent-removal/measurement gates are not complete.


M1C migration experiment verified on disposable databases: five tests pass, with real stage/restore/restage history, empty history, separately simulated uncertain and pending-intent states, four injected transaction rollback points, exact rowid/order preservation (including gaps), and independent refusal/preservation of unknown objects, future versions and malformed typed IDs/digests. Strict Clippy/fmt and frontend build/version guard pass. This is not runtime migration or process-crash/WAL durability proof, and it exposes no purge API. The last native application verification remains0.0.7; the0.0.8 changes add the isolated probe, matching version fields and developer version checks only. M1C and M1B remain In progress.


PR7 contains the isolated migration experiment (b2a9984). A proposed fixture-purge contract now describes receipt-only low-level APIs, exact separate review/confirmation, unchanged non-executable general-editor authority, ledger/intent ordering, uncertainty/restore rules, residual unlink races and separate space observations. It is not implemented or frozen. No permanent-removal API or runtime migration is enabled.


Autonomous work paused at the requested quota boundary: Codex usage reached76% (24% remaining), and the heartbeat was set toPAUSED. The isolated0.0.9 runtime-schema work passed131 workspace tests and five standalone migration tests plus strictClippy, but remains a local checkpoint pending final review and native startup verification. No purge API or user-file cleanup is enabled. Resume state and exact worktree/next steps are in .autonomous/disk-steward/progress.md.


## M1C follow-up — 0.0.15

Batches A–E implement selective receipt-based purge, backend-owned approval and native confirmation, conservative cross-store recovery, subprocess persistence tests, and the demo UI. PR15 merged with all five checks passing. Main still needs the D/E integration and the validation follow-up. M1C is not complete: controlled-volume measurement and the seeded real-app migration check remain open; M0 and M1B human/credential gates remain open as recorded in their validation documents. The earlier quota-pause entries are historical; Adrian authorized continued implementation and automatic merging of green PRs on 2026-11-02. No release or user-file cleanup is authorized.


## M1C verification probes — 0.0.15, 2026-11-02

PR16 is merged to main. The seeded migration gap is now verified through a uniquely identified real debug app: exact bundle/PID/open-store checks preceded the fixture-panel status request, schema advanced 1→2, and complete row/rowid/fixture-byte/receipt verification passed after that process exited. A standalone controlled APFS measurement run also passed on its own fully allocated 256 MiB fixture; ten probe tests and Ruff cover approval, space-margin, noisy-result and cleanup refusal paths. Evidence and remaining limitations are in docs/M1C-VALIDATION.md. Product-adapter observed-space reporting and human gates remain open. Recurring autonomous continuation is active; no release or user-file cleanup is enabled.


## M1C observed-volume accounting — 0.0.16

PR18 merged into main61b934b after current-head build/test jobs passed. The disposable macOS purge result now has an optional signed volume-wide observation, measured through the verified opened holding-directory handle after confirmation/authority checks and after durable completion. Missing or unreliable readings do not change execution. UI and tests keep that value separate from logical bytes. The actual native method and real coordinator path passed an owned-volume run; normalized evidence and limits are in docs/M1C-VALIDATION.md. M0/M1B human and credential gates remain open, and no user-file cleanup or release is enabled.


2026-11-02 — M1B pilot instrumentation (0.0.17): explicit local recording in the native synthetic pilot profile now tracks monotonic elapsed time, aggregate input/completed-command counters and initial/final aggregate context. Reports stay in renderer memory and contain no paths, names, raw IDs or key values; no export IPC/storage/network authority was added. Profile changes are locked during recording. Human assessment remains not_recorded and executionAvailable false. Moderator observations and three representative participants are still required.

Validation: 36 frontend tests, TypeScript/production build, Prettier, version policy, dependency audit (zero vulnerabilities) and diff checks passed. Unique migration-probe macOS app 0.0.17 started with an owned disposable HOME, exact PID/executable and open control-store paths verified, empty stderr. Both bundle-ID and exact-bundle-path UI attachment returned cgWindowNotFound; native recorder interaction is UNVERIFIED. Only the exact owned PID was terminated. No human pilot, native accessibility/performance or milestone pass is claimed.
