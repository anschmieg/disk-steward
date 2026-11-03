# M0 cumulative ingress accounting

Bridge cumulative transport observations into the pure shared budget controller without double charging repeated reports. New record_ingress_total(job,total) uses observed monotonic absolute bytes in the same attempt/domain; records only uncharged delta via existing limit logic. Reject unknown/stopped/completed jobs as before. A regressed counter leaves usage unchanged, fails a Running job closed with explicit IngressCounterRegressed, and preserves earlier Failed/Canceled states. Late increasing totals after failure/cancel remain charged until confirmed worker stop.

Root integration tests use actual private TLS pre-finalization and final totals, repeat observations, cancel between tail reads, and confirm charge retention/no cleanup eligibility. No acknowledgment of worker stop or cleanup is inferred. No live worker/store integration, durable aggregate or full Git verification claim.

Branch feat/m0-cumulative-ingress-accounting atop PR48 48d3e38; main finalbaseline afba4f6 through stack. Luna owns verification-budget lib.rs method/testsource only; root candidate test-only dependency/lock/integration tests/README/scope/progress/review before execution. No duplicate agents/overlapping builds. Host7.6GiBfree, no cachecleanup. Focused tests/Clippy/fmt/version0.0.17/diff/current-headCI; watched40–48 notpolled.
