# M0 payload allocation accounting

Correct the closed owned-store payload probe's length-only accounting. On supported Unix targets, compute checked st_blocks*512 and charge monotonic high-water max(logical length, allocated bytes) rather than logical growth alone. Observe before/after writes and after sync, retain charges on partial/error paths, preserve first budget failure and no cleanup authority. Logical fixture cap remains32KiB. No new CLI/renderer/filesystem permissions or user paths.

This accounts for the single payload file only; directory/journal/SQLite/pack index overhead, filesystem compression/clones/shared extents/delayed allocation and product whole-store budgets remain open. Reported blocks are allocation observations, not promised reclaimed bytes. Preflight remains advisory and cannot reserve space.

Bounded Luna owns payload.rs implementation and focused tests; root reviews concrete code before execution, docs/progress/integration/validation. Branch atop534621913/main02095ae baseline, preserve localnotifications; no watchedPRpoll/nooverlappingbuilds/CARGO_INCREMENTAL=0/2GiBreserve. Host9.3GiBfree. Version0.0.17/no releases.
