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
