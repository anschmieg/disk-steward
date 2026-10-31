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
