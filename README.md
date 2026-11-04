# Disk Steward

Working name; no naming or trademark research performed. Implementation is in progress at 0.0.17. The current desktop app demonstrates an editable synthetic plan and a review/confirmation/stage/restore/permanent-removal cycle for five disposable files it creates itself. Native file and folder selection can add a blocked metadata-only observation; separately confirmed folder exclusions can be saved for future scans. The desktop Explore folder view can inspect up to 512 immediate entries per page of a natively selected folder using metadata only; full resumable scanning and user-file cleanup are not enabled.

## Product

A Tauri desktop application with a shared Rust core that explains disk usage, investigates cleanup opportunities, and turns them into an editable plan. Ship macOS first; add Windows and Linux through validated platform adapters. Users can change decisions, add or remove items, inspect evidence, and approve an exact plan before execution. Discovery can run unattended; destructive operations cannot.

The central screen links a storage visualization, an editable action list, and an evidence inspector. The same item selection drives all three. The app distinguishes data that is large, potentially disposable, and actually selected for removal.

## Read the specification

Start with the [implementation roadmap](ROADMAP.md) for milestones, demonstrable outcomes, dependencies, completion criteria, and post-MVP tracks.

1. [Requirements](.kiro/specs/macos-cleanup/requirements.md): behavior and acceptance criteria.
2. [Architecture and interaction design](.kiro/specs/macos-cleanup/design.md): components, data contracts, Git analysis, Jev integration, security boundaries, and UI.
3. [Implementation tasks](.kiro/specs/macos-cleanup/tasks.md): dependency-ordered work and release gates.
4. [MVP detector and integration catalog](MVP-CATALOG.md): named tool/category targets, minimum executable scope, private Git authentication, and verification resource budgets.

These documents were prepared together because the request explicitly asks for the complete implementation plan. Implementation is authorized; cleanup of user data still requires an exact reviewed plan.

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

The earlier 12–16-week envelope is unvalidated and no longer an active delivery forecast. Reforecast after M0 and again after M1A from measured effort and frozen scope. Windows/Linux production releases remain separate tracks. M0 remains in progress. M1A's five-file disposable lifecycle is complete from its tested contracts; M1B is in progress; M1C is in progress; M2–M6 remain planned.

## Development

Version: 0.0.16. Repository: https://github.com/anschmieg/disk-steward (public). Changes use feature branches and reviewed pull requests into `main`. See [progress](PROGRESS.md) for verified results and remaining gates.

Use `npm ci` then `npm run dev` for the fixture UI, or `npm run tauri dev` for the desktop shell after installing the native prerequisites. `npm test` runs the frontend component tests; `npm run build` checks TypeScript and builds the frontend. `cargo test --workspace` tests the portable contracts, including the local-only gix transport seam; target that probe with `cargo test -p disk-steward-gix-byte-probe`. Run workspace formatting and lint checks with `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`. `cargo check --manifest-path src-tauri/Cargo.toml -j 2` checks the native backend separately. `node scripts/check-version.mjs` enforces matching 0.0.x versions. The optional evaluation harness is development tooling: see [evaluation notes](evaluation/README.md).

The current UI edits a backend-owned five-action synthetic plan, saves immutable review snapshots, and preserves decisions across restarts. A separate Fixture demo creates five new files and supports exact review → native confirmation → recoverable stage → results → fresh review/confirmation → restore. Metadata reviews cannot authorize those operations. Staging/restoring frees no space. Staged demo receipts can be separately reviewed and permanently removed after native confirmation. Results report logical bytes unlinked and, when available on macOS, a separate signed volume-wide free-space observation. The observation does not attribute space recovery to particular files. Crash recovery blocks automatic retries when an outcome is uncertain. The native backend exposes narrow typed commands, with no generic filesystem, shell or SQL access. It cannot scan or clean user files.

See [M1C validation](docs/M1C-VALIDATION.md) for purge and migration evidence and remaining measurement gates, [M1A validation](docs/M1A-VALIDATION.md) for observed native behavior, tests and remaining gates, and the [implementation forecast](docs/IMPLEMENTATION-FORECAST.md) for the revised work estimate. M1A is complete within its documented disposable scope; M0 remains open and no supported release is implied by a successful demo.

The native synthetic pilot workspace includes opt-in **Review pilot recording** controls. Enter an anonymous code, start before the tasks, then finish to inspect the local aggregate JSON report. Manually preserve it before closing the tab if needed. No report is sent or saved automatically. A moderator must separately assess preservation, comprehension, confusion and assistance; the recorder does not certify a pilot pass.

## Initial folder explorer

In the macOS desktop app, Explore folder opens a native folder picker and shows immediate entry names, kinds and exact logical byte sizes. Sort by name or logical size. The observation is bounded to 512 entries and cooperative two-second checks between OS calls; slow OS calls can exceed that interval. It does not descend into folders, follow entry links or read file contents. Nested sizes, unique physical allocation/reclaimable space and changes after inspection remain unknown. Hard links are counted per name, incomplete coverage is explicit, and all entries remain unavailable for cleanup. Canceling or failing a new observation preserves the previous result with a status message.

This is an initial inventory contribution, not resumable recursive scanning, detector eligibility, M2A completion or MVP acceptance. Delivery now prioritizes the usable MVP flow; once the roadmap MVP is produced, the automation must perform an adversarial review, fix findings and validate, then disable itself and stop. See [.autonomous/disk-steward/mvp-delivery-priority.md](.autonomous/disk-steward/mvp-delivery-priority.md).

The folder explorer also supports explicit drill-down into observed child folders and Back navigation, with display-only breadcrumbs. Navigation is confined to a backend-held native selection: the renderer supplies observation/entry IDs, not filesystem paths. Child identities are rechecked and links, volume crossings, hidden folders, recognized package/library boundaries and excessive depth are blocked. Navigation refreshes the immediate-entry observation; it does not recursively calculate folder sizes. Failed navigation keeps the previous observation. Sessions are memory-only and do not authorize file changes.

A local name filter narrows only the displayed entries. Subtle size bars retain the full observation's per-name regular-file logical-byte denominator when filtering/sorting, with exact byte labels always available. They do not represent allocated or recoverable space.

Navigation sessions are bounded to 16 held directory levels and 512 observed immediate entries. Boundary detection currently recognizes specific package/library name suffixes; it is not complete platform library/cloud-placeholder classification. Binding checks before and after scans are conservative race detection, not an atomic guarantee against another process continuously renaming directories. No file contents are read, and observations cannot authorize cleanup.

## Review observed entries

After exploring a folder, explicitly add the observed file/folder entries to Your plan. This saves bounded names and historical metadata locally in a separate initially empty workspace; pilot/stress examples stay separate. Filtering the explorer does not narrow the import. The whole batch is one undo step, and decisions, redo and saved review snapshots survive reopening. Reimporting the same observation is refused atomically.

Imported entries default to Keep and remain blocked from cleanup, including when Include is requested. There is no source-path binding or execution approval. Different observations may overlap or describe hard links: plan totals count per-entry logical metadata, not occupied or recoverable space. Unsafe integer sizes or cumulative totals are refused rather than rounded. Refreshing the explorer does not overwrite saved decisions.

The explorer now shows exact per-regular-file reported allocated bytes and link counts from no-follow filesystem metadata, separately from logical length. Allocation sorting and a keyboard-accessible evidence inspector preserve unknown values for folders, links and unsupported entries. Allocation is not summed into occupied/reclaimable totals: hard links and shared storage can overlap, and no deletion outcome follows from block metadata. Invalid or overflowing block observations stay unknown. Saved review evidence includes these observations without adding execution authority.

## Large immediate folders

Use Next page to continue enumerating a selected folder through a backend-held iterator, up to 512 entries per response and 128 pages per directory. Only the current page is retained in the list and eligible for metadata import; logical totals and size bars refer to that page. The cumulative count describes observed entry records, not distinct files or occupied space. An exact-full final page may conservatively offer one empty end page. Directory changes or uncertain continuation close the cursor and require a fresh folder selection. There is no automatic restart or hidden retry. Paging is memory-only, not a durable scan checkpoint; recursive scanning, pause/cancel and cloud/library acceptance remain open.

The macOS explorer now recognizes the SDK `SF_DATALESS` metadata flag. Known dataless directories are refused before opening/enumeration, with held metadata rechecked for navigation and continuation; the list explains dataless, volume, hidden-folder, managed-boundary and depth-limit blocks. Absence of the flag remains unknown content availability, not proof of local content. Dataless file observations retain explicit plan blockers. No hydration API or file-content read is introduced. These are metadata policy checks, not validation of real iCloud/File Provider behavior or atomic protection against flag changes between system calls.

The explorer retains its current in-memory folder/page/filter/inspection context when switching to the review plan or examples, without another picker or rescan. Clear folder scope explicitly releases retained directory/cursor ownership and cached observations after an exact-observation check; stale or busy requests preserve the current session. Saved plan and review metadata remain. This does not preserve the directory session across app restart or constitute a durable scan checkpoint.

Summarize subtree metadata explicitly walks the current backend-held folder through independent directory iterators, bounded to 4,096 entry records, 16 levels and cooperative two-second checks. It skips hidden entries, links, nonregular objects, volume crossings, recognized managed/system boundaries and known dataless entries. Coverage gaps and changed/unreadable directories remain explicit. File-object logical bytes deduplicate observed device/inode identities within this walk; they are historical metadata, not a consistent filesystem snapshot, occupied space or reclaimable space. Summary totals neither change current-page bars nor add plan items. No file contents are read, and this bounded observation does not close full scan acceptance.

Explicit subtree summaries now verify remembered exclusion bindings before and after observation. Matching directory identities are skipped and counted separately; direct selections within excluded physical ancestors are refused, including path aliases. Stale, inaccessible or changed saved rules conservatively refuse a summary until reviewed. The immediate-page metadata view remains separate and does not claim this scan-policy enforcement. These cooperative checks are not atomic filesystem isolation or full scan acceptance.

A running subtree summary offers cooperative cancellation bound to its exact observation and unique attempt. Requesting cancellation does not release busy ownership: the UI waits for the native result and preserves the page/cursor. An in-flight filesystem or native metadata call may still finish. This control is not pause/resume, durable checkpointing or a hard interruption guarantee.

Refresh current folder retries metadata access through the retained native scope without reopening the picker. A successful refresh starts page one with a fresh observation and clears the old subtree summary/filter/inspection; saved plan decisions and review metadata are unchanged. A failed refresh keeps the prior historical view. Replaced or inaccessible scope bindings may require a new native selection. This is an explicit retry, not durable scan restart.

Save folder checkpoint explicitly persists a stable incomplete page’s ordered raw entry-name prefix and native directory identity in private local app storage. It saves no source path or file contents. Resume saved folder scan works after restart only after you select the same unchanged folder again; strict identity, exclusion-revision and entry-order replay checks precede the next page. Changed folders, unavailable rules, filesystem reordering or resource limits refuse resume rather than guessing. This checkpoints one folder’s paging, not recursive traversal. Saving again explicitly advances the checkpoint; forgetting removes the checkpoint row without altering source files or saved review decisions and does not promise secure erasure. The first version caps prefix/JSON/database sizes and preserves the free-space reserve; full scan restart/performance and power-loss acceptance remain open.
