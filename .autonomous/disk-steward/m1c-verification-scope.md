# M1C verification probes — 2026-11-02

Purpose: strengthen evidence for the disposable removal lifecycle without expanding executable cleanup to user files. Baseline main f326a1b (PR16); work branch feat/m1c-verification-probes. Version stays 0.0.15; no release.

Desktop migration: build an external development configuration with a unique bundle identifier, product name and window title. Seed only a marked temporary HOME under the matching fixed probe app-data identifier. Verify the exact process and unique app before UI interaction, then require schema v2 and unchanged rows/rowids/fixture bytes. Generic display-name selection is prohibited after the inconclusive prior attempt. Production startup and renderer capability boundaries remain unchanged.

Controlled measurement: development-only probe creates its own bounded disk image and incompressible fully allocated 256 MiB fixture; no arbitrary input volume, no user data, no snapshot deletion or forced detach. Stage/restore/restage precede a separate exact-target confirmation. Observe signed free-space changes separately from logical unlink and allocation. Proposed tolerance max(16 MiB, 10% allocation), bounded 30-second observation. Coordinator must review concrete implementation before mutation. Host free-space reserve enforced; current free space approximately 5.8 GiB.

Failure cases: ambiguous app/process/data path, mismatched probe identifier, preexisting/symlinked directories, volume identity changes, sparse/compressed/cloned fixture, unrelated entries, low host space, failed detach, stale confirmation, changed fixture identity, or noisy/nonconverging free-space observation. Fail closed and retain evidence. A standalone native probe does not establish the product adapter measurement gate or human usability gates.
