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
