# M0 live store/worker binding

Purpose: connect an exclusively created probe store to the exact fixed child spawned by the Rust supervisor. Add a separate --run-store-worker case; retain existing stdout-only and store-inspection cases. Persist created→bound(actual PID)→stopped(actual PID), and issue a private typed receipt only after successful child wait and reader join. A receipt for another token/PID or a replay must not change a store.

No cleanup API, source path, extra child mode, filesystem-writing child, network, credential, renderer or restart-adoption authority. All fixtures retained. Inspection remains NeedsReconciliation even after a locally proven stop. Before journal updates revalidate fixed marker content/metadata/root identity; interrupted/failed writes poison the handle or refuse further mutation. No claims of transaction/power-loss durability or production containment.

Root must review actual mutation and shutdown paths before tests execute. Validate binding/replay/mismatch refusals plus actual subprocess cancellation/join, fmt, strictClippy and current-head CI. Version0.0.17; branch basedPR27head77b93af, finalintegrationmain afterbaseline. Existing PR27watchnotpolled.
