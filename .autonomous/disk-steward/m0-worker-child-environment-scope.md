# M0 closed worker environment boundary

Clear inherited environment at all four existing fixed current-executable launch sites: generic supervision, store worker, creator-death reinspection and indexed store worker. Existing fixed modes, arguments, stdio bounds, deadlines, child reap/reader joins and approval boundaries remain. No new executable lookup, child mode, caller path, credential, network, renderer or cleanup capability.

A fixed synthetic DISK_STEWARD_WORKER_ENV_SENTINEL is rejected by the private dispatcher without echoing its value. Integration negative control directly invokes the existing child mode with that variable; all four parent modes must succeed with it set, proving actual launches discard it. This does not prevent a caller manipulating the parent before startup, prove loader/sandbox/process-tree isolation, or integrate the production Git worker/store protocol.

R5.8/M0 worker-boundary contribution. 32 unit and 11 integration tests pass locally macOS, including reservation/reconciliation/identity/confirmed cleanup boundaries; strict offline all-target Clippy/fmt/diff pass. Linux current-head CI remains required. Free space observed 3.2 GiB before validation, no user caches removed or overlapping builds. Version0.0.17/no release/no milestone completion. Branch atop79, eventual main integration requires current-head checks and no blocking findings.
