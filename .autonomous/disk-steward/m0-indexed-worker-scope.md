# M0 indexed fixed-worker fixture

Purpose: connect the durable metadata registry to an actual owned fixture and confirmed Rust child lifecycle. Separate closed command creates its own store/control DB in the generated private parent, registers actual descriptor identities under a fixed fixture-only ID, records active after spawn/bind and stopped only after actual wait/reader proof, then releases/reopens the registry and requires NeedsReconciliation/priorStopped with unchanged identity.

No caller paths, store discovery, new child modes, renderer, network, credentials or cleanup. Reopened metadata cannot create a receipt, adopt a store or authorize retry. Indexed and cleanup modes must not combine. Errors after spawn must stop/reap before returning. Fixed fixture ID is not a production ID allocator. All artifacts retained.

Root reviews concrete code before any new fixture tests. Validate real process path, index reopen, retry refusal and guards, formatting/strictClippy/version/current-head CI. Version0.0.17, branchPR32head6bcd62c; finalmain afterbaseline. ExistingPR32watchnotpolled. No production startup/Git/M0 completion claim.
