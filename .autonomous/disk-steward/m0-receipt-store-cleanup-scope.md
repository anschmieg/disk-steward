# M0 receipt-gated owned fixture cleanup

Purpose: prove removal of the Rust probe's own fixed journal and empty store directory only after exact child shutdown. Separate CLI requires --run-store-cleanup --confirm-own-store-cleanup before any fixture creation. Reuse current exact own-child/reader shutdown and live store-bound StopReceipt; no reopened journal can authorize cleanup.

No recursive removal or caller paths. Validate parent/root/marker identities, ownership, modes, exact prior contents and directory entries. Unknown root/parent entries refuse before mutation. Persist exclusive cleanup intent outside store in its private parent, remove only recorded owner.journal and empty fixed store through directory FDs, sync and record completion. Keep private parent+cleanup record. After intent, failure poisons handle; no blind retries/adoption. No temporary-output, Git-store, user-file, renderer or recovered-byte claim.

Only freshly created task-owned disposable fixtures are mutated; parent reviews concrete code BEFORE cleanup tests execute. Test actual child proof and authority/identity/unknown-entry refusals. Source review plus tests are not process-tree/hostile-same-user-race/power-loss durability proofs. Version0.0.17, branchPR29head397056e, finaltargetmain afterlowerlayers/currentheadCI. ExistingPRwatchesnotpolled.
