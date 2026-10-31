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
