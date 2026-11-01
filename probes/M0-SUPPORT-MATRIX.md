# M0 support and integration decisions

Status: provisional technical decisions, 31 October 2026. This matrix records what the experiments justify building next. It does not certify production operations or close M0.

| Boundary | Implementation direction | Evidence and remaining gate |
|---|---|---|
| Shared application | Tauri 2, React/TypeScript, Rust domain services, bundled SQLite | Native debug app and read-only IPC work. Cross-platform builds still need runnable CI; no unsupported OS cleanup adapters. |
| Access model | Direct-distributed macOS app, selected folders first; no elevated helper | Unsigned CLI bookmarks are insufficient evidence of packaged access. Local signing inventory returned zero valid code-signing identities. Developer ID/notarization, denial/revocation and selected-folder persistence remain open. Do not infer scope from a prompt or path string. |
| Holding | Use app-managed same-volume holding for the first lifecycle slice; native Trash remains an investigated alternative | Native no-overwrite Rust rename and interrupted temporary staging passed fixtures. Trash experiments preserved the unrelated sentinel. Production actions still need durable exact receipts, complete revalidation and interruption/permission gates. See docs/decisions/0001-recoverable-holding.md; no cross-volume fallback. |
| Approvals | Immutable backend manifest plus native confirmation; SQLite single-use consumption and one attempt per approval | Eight ledger tests passed. M1A now integrates typed IDs/digests, exact native confirmation and the one-time ledger for app-created fixtures only. Started intents are records requiring reconciliation, never automatic retries. |
| Verification | Isolated app-owned worker/store, fixed transport, continuous accounting | Pure budget accounting and bounded HTTP reads passed. Actual Git ingress/temp/free-space/time enforcement, interrupted-store ownership and credential mediation remain gates. Socket timeouts are not hard deadlines. |
| Credentials | macOS Security framework, backend broker only | Unique synthetic Keychain item passed create/read/delete. Real app token access, revocation and identity/access groups remain unproven. |
| AI | Optional provider adapter; no model route can grant mutation authority | Four synthetic development requests established reachability/schema/cost only. Independent human labels and meaningful development evaluation precede frozen evidence contracts. Held-out scenarios remain reserved. |
| Generated-data candidates | Prioritize D2 CPython 3.15.0 checked-hash bytecode and D3 pytest 9.2.0 recognized nodeids record | These versions produced representative fixtures; the coordinator replayed both in an isolated environment on 1 November and retained the result JSON. Decision0002 selects them as M3 implementation targets, not enabled adapters. Execution eligibility requires non-executing provenance checks, source/survivor retention, active-use policy and negative/recovery suites. Other versions are unknown, not implicitly compatible. |
| Remaining offline catalog | Exact duplicate D6 remains required; D1 Cargo/D4 Homebrew remain investigation targets | No downgrade of the agreed catalog is declared here. Unknown/mixed content stays protected. Final executable subset and review-only scope must be explicitly frozen before M0 passes. |

## Next integration sequence

1. Finish worker cancellation and app-level access/confirmation feasibility. Record actual blocked cases, including permission denial and interruption.
2. Select and prove a bounded private Git transport/credential broker against disposable repositories. Advertisement reachability alone does not prove remote object completeness.
3. Complete independent scenario-label review and bounded development evaluation. Record mistakes and evidence inadequacy rather than tuning against held-out cases.
4. Freeze the supported detector roots/versions/provenance checks and recovery constraints. Record the remaining engineering work and a revised forecast.
5. Begin M1A's complete five-action lifecycle only with explicit unresolved gates and no production destructive authority beyond validated scope.

Human usability pilots and release signing are later acceptance requirements; automated tests cannot substitute for them. Local passing checks do not substitute for currently unavailable CI. Versions remain 0.0.x, and no release is published automatically.
