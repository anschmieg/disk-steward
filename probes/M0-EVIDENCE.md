# M0 feasibility evidence — 31 October 2026

These are coordinator-observed development experiments, not production adapter certification. All write/delete experiments used self-created disposable fixtures. Production capabilities remain unsupported in the application.

| Area | Observed result | Still unproven |
|---|---|---|
| Native application | A local debug Tauri app bundle built and launched. Native UI showed `Native core connected · 0/7 production adapters available`; list navigation worked. | Developer ID signing/notarization, full permission matrix, distribution and supported-version coverage |
| Native folder chooser | Packaged 0.0.3 debug app opened a new empty fixture folder read-only; second chooser cancellation reported no retained access. No entry names were consumed by the backend check. | Persistent scope grants, denial/revocation, sandbox variants, distribution signing and race-resistant authorization |
| Parented native review | Tauri preview displayed the fixed backend summary as a native sheet. Explicit Cancel and Escape both returned canceled. | Production immutable manifest/digest binding, approval issuance, final executor validation and broader permission/keyboard matrix |
| Read-only bridge | `foundation_status` is registered with an explicit application permission; it accepts no paths/commands/credentials and reports backend-owned core capability state. | Future scan/plan/execution command authorization and durable workflow integration |
| Native no-overwrite rename | Six native macOS Rust fixture tests passed: destination conflict preserved both sides, stage/restore kept inode/device, no-follow/component checks rejected links/traversal. Ambiguous post-move outcomes have a distinct reconciliation state. | Linux/Windows runtime evidence, hostile swaps, production scope/receipt/content integration and filesystem matrix |
| Native identities | Foundation volume/resource identity survived same-volume stage/restore; collision and symlink cases were rejected; SHA-256 detected same-size edits. | Elimination of path races, hostile concurrent changes and crash durability |
| Native Trash | Restricted execution returned Cocoa 513. The access-enabled disposable experiment staged/restored its exact returned URL. Opt-in purge removed only a new target receipt; the separate synthetic Trash sentinel survived and was restored. | Production entitlement/access behavior, arbitrary content/volume support and persistent receipt reconciliation |
| Controlled APFS recovery | An isolated 4 GiB test image completed stage/restore/restage/purge. Stage and restore free-space readings stayed constant. Allocated fixture bytes: 276,824,064; observed recovery: 276,832,256; tolerance: 27,682,406. Only the probe image was detached/removed. | Results on normal snapshot/shared/busy user volumes; no snapshot removal was attempted |
| Git HTTPS reachability | Public Tauri advertisement: HTTP 200, 641,593 bytes. Private app-repository advertisement: HTTP 200, 644 bytes with existing credentials in memory; anonymous private access failed. A subsequent synthetic-invalid credential was rejected with HTTP 401. No source clone was changed. | Full object/ref proof, app-owned credential broker, revoked-token UX, worker hard deadlines and live temporary-store enforcement |
| Keychain | Security-framework helper created/read/deleted one UUID-named synthetic item and verified absence without authentication interaction. No existing item was enumerated or read. | Packaged-app real-token integration, rotation/revocation and permission acceptance |
| Jev | Four synthetic development-only requests succeeded on `typesafe/jev-1.14-20261014`, total reported cost $0.000087444. No labels, held-out cases, disk contents or credentials were stored in results. | Human-reviewed labels, routing correctness/usefulness, held-out evaluation and provider-side hard spend controls |
| Durable approvals | SQLite reopen, exact digest/expiry, atomic two-connection consumption and one-intent-per-approval passed six tests. Altered schemas are rejected. | Connection to typed core, production permission checks, process-kill fault injection and journal/filesystem reconciliation |
| Scope bookmarks | Ordinary fixture bookmark resolution and replacement-identity detection passed; security-scoped creation failed in the unsigned CLI. | Packaged selected-folder access, revocation/denial matrix and restart grants |
| Worker supervision | Disposable fixed child writer was stopped and joined before store cleanup; allocated-growth overshoot and deadline overshoot were measured. Forged store/receipt and symlinked-root cases were rejected. | Actual Git transport ingress enforcement, Rust production workers, crash durability and continuously maintained free-space reserve |
| Interrupted staging | Real disposable child stops before rename and after rename/before outcome reconciled to original/staged with durable intent. No move was retried. Changed contents, collisions, tampered receipts and replaced parent folders blocked restore. | Native Trash interruption, power-loss durability, hostile path races and production journal/approval integration |
| Git transport API | Actual gix blocking/HTTP traits compiled with synthetic refs/pack responses and one aggregate atomic response-body budget. A simultaneous-body regression caps total exposure. | Concrete network backend, headers/TLS/socket-prefetch accounting, hard worker deadline, authentication and full remote object proof |
| Generated data | CPython 3.15.0 checked-hash bytecode regenerated exactly; missing source, altered bytes, symlink and unexpected-file cases were rejected. A disposable environment with pytest 9.2.0 produced a recognized `nodeids` cache record. | Production detector implementation, broader versions, regeneration/tool discovery and full cache-record eligibility |

## Corrections made during review

- Budget observations beyond a limit remain charged; failed/canceled workers retain their concurrency slot until stopped acknowledgement. Cleanup cannot release their accounting early.
- HTTP streaming uses bounded reads and post-read deadline checks. DNS/connect still need a production worker-level hard deadline; the probe does not claim otherwise.
- The image probe originally used an invalid image-type argument, then an explicit byte size that the installed compatibility command rejected. Installed help and a disposable diagnostic established `-type UDIF -size 4g`; that form passed. macOS 27 warns that the compatibility command is deprecated. The probe is development tooling, not the shipped app's storage implementation.
- Catalog tests were initially missing from recursive discovery because their package marker was absent. Package imports/discovery were corrected; they are included in the full probe suite.
- Jev's first result was `keep` where the provisional fixture label expected metadata inspection. Route semantics were clarified before three further cases. Four calls do not establish accuracy or justify action eligibility.

## Interpretation

The native experiments support continuing with receipt-based recovery feasibility, a shared typed core and OS-specific adapters. They do not close all M0 gates. Human evaluation, production broker/resource integration, detector scope/version freeze, durable storage approach, native confirmation/permission probes and the forecast must remain explicit.

The live evaluation's cost threshold is a post-response stop, not a hard provider-side billing cap. Request counts/input size were bounded and only four small synthetic requests were made. Never present these provisional labels or rule-matched baseline scores as an independent safety benchmark.


## Catalog follow-up — 1 November 2026

Luna narrowed the D2/D3 implementation target and expanded negative fixtures. Coordinator replayed the updated probe with CPython3.15.0 and pinned pytest9.2.0 in an isolated temporary environment after the restricted install failed; the environment was removed afterward. Both manager-generated fixtures passed. The retained result is probes/catalog/evidence/2026-11-01-fixture-replay.json. Six catalog tests and45 then-current Python probe tests passed; unmatched interpreter versions are explicitly skipped and unknown pytest versions do not execute the fixture. This is feasibility, not production ownership/activity or full cleanup support. Decision0002 preserves the separate D5/D6 gates; no user files were touched.

A read-only signing inventory with normal host access returned zero valid code-signing identities (exit0). Signed/notarized packaging cannot be validated on this host yet. It does not block independent local implementation, and no identity or credential was installed or changed.
