# Requirements

## Scope and priorities

P0 requirements are required for the initial public release. P1 denotes subsequent capabilities, unless explicitly included as read-only discovery. Defaults favor explicit decisions and honest uncertainty. Scan completion does not establish removability.

## R1 — Scope, access, and coverage (P0)

User story: I can choose what is scanned and understand what the app could not inspect.

- R1.1 WHEN a user starts a scan, the app SHALL enumerate selected local scopes without following symbolic links or crossing volumes implicitly.
- R1.2 The app SHALL record inspected, excluded, inaccessible, unsupported, and pending areas separately; it SHALL NOT label inaccessible areas clean.
- R1.3 WHEN permissions change, the app SHALL retry affected scopes without requiring a full restart.
- R1.4 The app SHALL avoid reading cloud-only contents, secrets stores, managed library internals, and operating-system protected areas by default.
- R1.5 WHILE scanning, the app SHALL support pause/cancel, bounded concurrency, storage budgets, and restart from a durable checkpoint.

## R2 — Inventory, evidence, and accounting (P0)

User story: I can understand why an item was suggested and whether the estimated saving is credible.

- R2.1 Each finding SHALL link to inventory identities, a detector/version, recorded observations, unknowns, and evidence freshness.
- R2.2 The app SHALL distinguish logical size, allocated bytes, estimated reclaimable bytes, bytes staged for removal, and observed free-space change.
- R2.3 The app SHALL deduplicate overlapping actions and account conservatively for hard links, shared storage, APFS uncertainty, and recovery copies.
- R2.4 A filename, extension, ignore rule, or old timestamp alone SHALL NOT establish safe removal.

## R3 — Editable cleanup plan (P0)

User story: I can change every proposed decision before it affects my computer.

- R3.1 The app SHALL allow keep, remove from plan, supported cleanup action, and further investigation on individual items and groups.
- R3.2 WHEN a user adds a file/folder, the app SHALL inventory it and apply the same eligibility rules as discovered items; unsupported operations SHALL explain their blocker.
- R3.3 WHEN a user excludes descendants, changes a duplicate survivor, or changes an action, the app SHALL rebuild affected actions, consequences, and estimates.
- R3.4 The app SHALL keep user decisions separate from model suggestions; rescanning SHALL NOT silently overwrite a user decision.
- R3.5 The app SHALL support plan-local keep decisions and separately confirmed persistent exclusions, plus undo/redo of plan edits.
- R3.6 The app SHALL present exact targets, action, rationale, effects, unresolved issues, and recovery limitations before approval.

## R4 — Approval and execution (P0)

User story: Only the exact operations I reviewed are performed.

- R4.1 Approval SHALL bind to an immutable plan revision, target manifest, adapter versions, preconditions, and expiration; changes SHALL require renewed approval.
- R4.2 The executor SHALL revalidate identities and relevant contents immediately before each mutation and skip stale or invalid actions.
- R4.3 The executor SHALL accept only supported typed actions; model text and arbitrary shell strings SHALL NOT be executable inputs.
- R4.4 WHEN canceled or interrupted, execution SHALL leave a durable completed/skipped/failed/unstarted record and reconcile uncertain outcomes before retry.
- R4.5 New discoveries SHALL NOT be added to an approved batch. Continuing a materially changed batch SHALL require a revised plan.
- R4.6 P0 removal SHALL use a validated recoverable holding mechanism, with native Trash labeled as such only when actually used. Permanent removal of exactly the app-recorded staged items SHALL be included in the MVP as a separately reviewed operation; it SHALL NOT empty unrelated Trash or run automatically when a retention period expires.
- R4.7 M3 SHALL demonstrate review → stage → restore → stage again → separately approve permanent removal → observe recovered space on controlled disposable fixtures. Merely moving bytes into a holding area SHALL NOT satisfy the cleanup completion criterion.

## R5 — Git repository analysis (P0)

User story: I can remove an old clone without accidentally discarding work that exists only locally.

- R5.1 The app SHALL identify ordinary repositories, bare repositories, linked worktrees, submodules, nested repositories, and shared object stores; unsupported layouts SHALL block clone removal.
- R5.2 The app SHALL examine staged/unstaged, untracked and ignored files, all local refs, detached HEAD, stashes, local metadata, and evidence of recoverable unreachable state.
- R5.3 Inactivity SHALL rank opportunities, not authorize removal; last commit time SHALL NOT be presented as last use.
- R5.4 A removal candidate SHALL require fresh remote evidence for relevant commits/refs and verified preservation or explicit treatment of local-only material; remote-tracking refs alone SHALL NOT suffice.
- R5.5 Offline, authentication-failed, uncertain-history, unsupported-protocol, and incomplete-object cases SHALL remain unverified and blocked.
- R5.6 The app SHALL disclose that remote accessibility now does not guarantee future remote retention.
- R5.7 The app SHALL separately measure working files, generated files, Git objects, and other Git state without double-counting a clone and its artifacts.
- R5.8 Remote verification SHALL enforce explicit network, temporary-storage, aggregate-storage, time, and free-space budgets; cancellation/crash cleanup SHALL affect only app-owned verification stores. Incomplete or budget-limited verification SHALL NOT authorize clone removal.
- R5.9 MVP acceptance SHALL cover public GitHub HTTPS and private GitHub HTTPS with app-mediated credentials stored in the OS credential store, including denied/revoked credentials. SSH and other unsupported transports SHALL have an explicit blocked state and SHALL NOT be silently rewritten or authenticated using arbitrary repository-configured helpers.

## R6 — Visual navigation and review (P0)

User story: I can see where space is used and understand the effect of my choices.

- R6.1 The app SHALL provide a labeled category distribution, drill-down storage map, sortable list, and file-type/size views linked to a shared selection.
- R6.2 Storage occupied, candidate savings, and selected actions SHALL have separate labels and representations; color alone SHALL NOT communicate meaning.
- R6.3 The plan SHALL expose ready, needs decision, blocked, kept, and stale states with textual explanations.
- R6.4 Keyboard navigation, VoiceOver descriptions, text/list equivalents, reduced motion, and light/dark appearance SHALL be supported.
- R6.5 WHEN scan results update, the app SHALL preserve focus and selection and avoid continuously rearranging the review surface.
- R6.6 List rows SHALL contain subtle proportional background bars behind their text, with exact sizes remaining readable. The default denominator SHALL be the containing folder's measured total, labeled in the interface and stable under sorting/filtering. Unknown sizes SHALL NOT be represented as zero. Selection and keyboard focus SHALL remain distinguishable from the bars.
- R6.7 The plan editor SHALL pass the representative review-throughput protocol in design.md, including a large plan, mixed blockers, descendant exclusions, user overrides, and a final comprehension check. Backend correctness alone SHALL NOT establish review usability.

## R7 — Optional Jev and reasoning integration (P0)

User story: AI can help investigate unfamiliar storage without receiving uncontrolled access to my data or disk.

- R7.1 The app SHALL operate without API credentials for supported detection, planning, and execution.
- R7.2 Remote requests SHALL require opt-in, disclose fields sent, redact identifiers by default, and use an OS credential-store adapter (Keychain on macOS); credentials SHALL NOT enter renderer state or browser storage.
- R7.3 Jev SHALL select only allowed routing/classification outcomes; its confidence SHALL NOT satisfy deletion prerequisites.
- R7.4 Investigations SHALL be bounded by item/time/token/request budgets and limited to typed inspection capabilities.
- R7.5 The app SHALL validate provider responses, fail closed on unavailable/invalid output, and record model/schema versions and evidence references.
- R7.6 Every executable AI-origin recommendation SHALL receive a distinct evidence-based review; missing review leaves it blocked. Deterministic user-selected actions do not require a model connection.
- R7.7 M0 SHALL include a bounded synthetic/explicitly consented AI feasibility evaluation; M5 SHALL re-evaluate on held-out cases. Results SHALL record routing correctness, unsupported-action suggestions, investigation usefulness, cost, latency, and the evidence format's limitations. Full production AI integration remains in M5.

## R8 — Recovery and privacy (P0)

- R8.1 The app SHALL record recovery mechanisms per action and SHALL NOT describe reinstalling/recloning as complete restoration of local state.
- R8.2 Restore SHALL handle original-path collisions explicitly and SHALL NOT overwrite newer files.
- R8.3 Diagnostic logs SHALL exclude secrets, content, raw remote credentials, and full paths by default. Inventory/report exports SHALL require explicit selection and disclose their sensitivity.
- R8.4 A resource-limited or disk-full scan SHALL stop cleanly and report incomplete coverage.
- R8.5 Holding records SHALL preserve exact staged identities and recovery locations. Permanent removal SHALL invalidate restoration availability and require fresh target checks and a separate approval; changed, missing, or user-moved holding items SHALL be reconciled rather than deleted through a stale path.

## R9 — Later Git preservation and maintenance (P1)

- R9.1 Commit/push SHALL require review of exact files/diffs, repository, branch, destination, message, and potential secret exposure; no automatic add-all or force push.
- R9.2 A successful push SHALL trigger new remote verification and a new removal proposal, never deletion under the previous approval.
- R9.3 Git maintenance SHALL use supported Git/LFS operations, preserve default recovery windows, and report measured results; branch deletion SHALL NOT promise immediate object-space recovery.
- R9.4 Archive/bundle preservation SHALL be independently verified, include a manifest of excluded local material, and account for its storage cost.

## R10 — Delivery (P0)

- R10.1 The app SHALL launch and perform its deterministic workflow without Codex, Node, Python, or a hosted backend.
- R10.2 The release SHALL be signed/notarized, expose missing optional tools clearly, and pass the execution/adversarial release gates in tasks.md.
- R10.3 Fixes SHALL normally be patch releases; meaningful additions or interaction changes minor releases; breaking contracts/major product changes major releases.
- R10.4 The MVP detector catalog and Git authentication scope SHALL be frozen at the M0 exit gate. Schedule forecasts SHALL be revised after M0 and the first complete M1 vertical slice; unsupported or removed scope SHALL be documented explicitly rather than silently counted as complete.

## R11 — Portability and renderer boundary (P0 architecture; later OS releases)

- R11.1 Shared inventory/detector interfaces, Git analysis, Jev/reasoning, planning, and journal logic SHALL compile without macOS-specific dependencies; OS operations SHALL use explicit platform adapters.
- R11.2 The renderer SHALL use typed domain commands and opaque item/action IDs; arbitrary paths, SQL, shell commands, provider credentials, or model-authored commands SHALL NOT grant execution authority.
- R11.3 The Rust backend SHALL validate every custom command and enforce manifest/approval checks independently of Tauri capability configuration and frontend state.
- R11.4 OS-specific action support SHALL remain disabled until that adapter passes permission, identity, race, interruption, recovery, and accounting tests on the target OS. A portable UI SHALL NOT imply supported cleanup.
- R11.5 The core SHALL preserve lossless OS paths, account for case sensitivity and link/reparse semantics, and keep permission/volume/Trash differences explicit.
- R11.6 The storage map SHALL encode allocated size by area and category by color, with separate selection and decision marks; map/list/plan views SHALL share selection and support an equivalent accessible list.
- R11.7 The decision view SHALL show bytes assigned to removal, further review, and keep, and update as individual decisions change. Movement between decisions SHALL NOT alter the occupied-size baseline or imply immediate free-space recovery.
- R11.8 Jev and reasoning requests SHALL receive a versioned platform profile describing the detected environment, relevant directory conventions, protected scopes, available collectors/actions, and category-specific investigation guidance. Prompt context SHALL NOT replace deterministic scope, identity, eligibility, or approval enforcement.
