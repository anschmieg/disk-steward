# Disk Steward — cross-platform cleanup application plan

Working name; no naming or trademark research performed. Planning package dated 31 October 2026. This package specifies an application; it does not install software or clean the computer.

## Product

A Tauri desktop application with a shared Rust core that explains disk usage, investigates cleanup opportunities, and turns them into an editable plan. Ship macOS first; add Windows and Linux through validated platform adapters. Users can change decisions, add or remove items, inspect evidence, and approve an exact plan before execution. Discovery can run unattended; destructive operations cannot.

The central screen links a storage visualization, an editable action list, and an evidence inspector. The same item selection drives all three. The app distinguishes data that is large, potentially disposable, and actually selected for removal.

## Read the specification

Start with the [implementation roadmap](ROADMAP.md) for milestones, demonstrable outcomes, dependencies, completion criteria, and post-MVP tracks.

1. [Requirements](.kiro/specs/macos-cleanup/requirements.md): behavior and acceptance criteria.
2. [Architecture and interaction design](.kiro/specs/macos-cleanup/design.md): components, data contracts, Git analysis, Jev integration, security boundaries, and UI.
3. [Implementation tasks](.kiro/specs/macos-cleanup/tasks.md): dependency-ordered work and release gates.
4. [MVP detector and integration catalog](MVP-CATALOG.md): named tool/category targets, minimum executable scope, private Git authentication, and verification resource budgets.

These documents were prepared together because the request explicitly asks for the complete implementation plan. Their existence is not approval to implement or perform cleanup.

[Initial desktop concept](concept-desktop.png) and [narrow-layout check](concept-mobile.png) show synthetic data. The revised visual direction is specified in design.md: a spatial storage map linked to an editable decision view and evidence inspector. Preview checks verify concept interactions only, not the proposed application or filesystem safety engine.

[Revised spatial concept](concept-spatial.png) and [decision view](concept-decisions.png) illustrate the updated approach. Folder areas stay constant while decisions and selected totals change; mixed folders expose their category composition instead of receiving a misleading single category.

## Proposed technical choices

- Tauri 2, React, TypeScript, and Vite for the shared desktop interface; D3 for treemap/partition layout and SVG charts, with Canvas only when profiling warrants it.
- Rust core for inventory, specialist detectors, Git analysis, Jev/reasoning orchestration, policies, and typed execution. SQLite through rusqlite with a serialized writer and bounded blocking jobs. Confirm supported versions and lock dependencies during implementation.
- Narrow frontend commands, backend-owned approvals, and isolated native workers where appropriate. No generic shell, SQL, or filesystem API exposed to the renderer; no root helper in the MVP. Tauri capabilities constrain IPC exposure, not the Rust process's operating-system privileges.
- Developer ID signed, hardened, notarized direct distribution. Begin with explicitly selected folders; optional broader access with clear macOS permission handling. Validate the sandbox/distribution tradeoff before committing to entitlements.
- Optional OpenRouter Jev adapter using its Decisions API, isolated behind a provider interface. Optional reasoning-model investigation. Offline operation remains complete for supported deterministic detectors.
- OS adapters implement scope access, file identity, allocated size, change notifications, credentials, Trash/recovery, package ownership, and execution semantics. Shared Rust code must never assume Unix paths or macOS cleanup rules.
- No runtime dependence on Codex, Node, Python, or a hosted application backend; Node is build tooling only. Use the platform WebView with platform prerequisites verified in M0. Git features use a discovered, capability-checked Git executable; absence disables those features with a clear explanation.

## MVP boundary

Ship the complete review → recoverable stage → restore if needed → separately reviewed permanent removal → observed-space workflow. Holding mechanisms are validated in M0 and labeled honestly; moving to Trash/staging alone is not cleanup completion. Permanent removal applies only to exact app-recorded staged identities, never unrelated Trash, and never runs on an automatic timer.

The [named catalog](MVP-CATALOG.md) targets Cargo compiler output, CPython bytecode, pytest cache, Homebrew package downloads, installer review, large-item discovery, exact duplicates, and ordinary Git clones. M0 freezes versions and eligibility; M3 needs at least two executable generated/cache families plus exact duplicate cleanup. Unsupported categories remain visibly review-only, not counted as completed cleanup features.

Git acceptance includes public and private GitHub HTTPS with app-mediated Keychain credentials, fresh remote/object verification and explicit local-state dispositions. Remote verification has enforced network/storage/time/free-space limits and abandoned-store cleanup. SSH/other hosts and complex layouts are visible but initially blocked from automated clone removal.

Do not include in-app commit/push, Git branch deletion, Git garbage collection, generic application uninstall, near-duplicate deletion, arbitrary recursive permanent deletion, or privileged system cleanup in the first release. These require additional adapters and acceptance tests, not just extra prompts.

## First implementation slice

Build a five-action synthetic plan → edit → frozen approval → recoverable stage → results → restore slice (M1A), then advanced editing/review-throughput validation (M1B) and selective permanent removal/space measurement (M1C). Deliver live scanner + list/background bars first (M2A), then the corrected rich map/distributions (M2B). M3 proves the full lifecycle with actual detector outputs.

Run a small bounded AI feasibility evaluation in M0 before freezing evidence contracts; full production Jev/agent integration remains M5 and uses held-out evaluation. Missing evaluation access is an explicit dependency, not evidence of success.

The earlier 12–16-week envelope is unvalidated and no longer an active delivery forecast. Reforecast after M0 and again after M1A from measured effort and frozen scope. Windows/Linux production releases remain separate tracks. All implementation milestones are Planned.
