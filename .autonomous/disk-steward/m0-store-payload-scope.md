# M0 fixed owned-store payload budget

Purpose: a closed `--run-store-payload-fixture` command writes deterministic nonzero bytes to one exclusively created `payload.bin` beneath a fresh task-owned private root. Attempt 64 KiB in at-most-4-KiB chunks; hard logical cap 32 KiB. Hold/pin the file FD and check the no-follow named identity, current UID, regular/single-link/0600 metadata, exact current length, root/parent and owner journal before writes. No caller paths, names, sizes or adoption.

Sample available bytes via root-FD fstatvfs with checked arithmetic. Require the existing 2 GiB minimum reserve plus probe-specific 1 MiB headroom and full cap before payload creation; repeat with next chunk before each write. This is sampled reserve gating, not a continuously maintained reserve against concurrent writers, allocated-space enforcement, aggregate-budget integration or calibrated production headroom. Partial writes/sync/identity failures retain artifacts and report incomplete/reconciliation. No power-loss durability claim.

Consume the public store handle; retain every file. Do not modify owner-journal format or cleanup logic: payload is an unknown entry and existing cleanup must refuse before any marker unlink or cleanup intent. No worker/process-stop, Git object, transport, credential, renderer, user data or new cleanup authority. Root source review before any fixture execution.

Branch atop PR37 head30df26e; main final baseline. Luna owns new store/payload.rs and module declaration only; root owns dependency/main/CLI test/docs/progress. Required behavior/format/strictClippy/version0.0.17/diff/current-headCI before merge. Existing35/36/37 watches notpolled. Reserve15GiB observed before work; no user-cache removal.
