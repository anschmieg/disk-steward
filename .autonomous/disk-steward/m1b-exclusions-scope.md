# M1B remembered-exclusion implementation boundary

Purpose: let users make a deliberate, durable preservation choice without silently hiding findings or confusing a changed path with the original directory. This continues M1B; discovery and application to scan coverage remain M2.

Implement a distinct versioned exact-directory exclusion registry. Native add selects a directory, records a private lossless locator and supported volume/object identity, asks a separate native confirmation, then revalidates before durable insertion. Removal likewise needs native confirmation and an expected registry revision. Renderer requests contain only typed IDs/revisions; public rows contain sanitized basename and unverified/stale/unavailable status, never raw paths, identity bytes or approval flags. No bookmarks or scan/cleanup grants are retained.

Private storage must bound rows/blobs, use exact SQLite schema ownership/NOFOLLOW, preserve unknown data, reject stale/conflicting bindings, and protect sensitive path bytes with restrictive permissions. Matching is a fresh adapter observation, never a persisted active flag. Path replacement, inode reuse, volume changes, unavailable identity and denied access remain unverified rather than silently matching.

Current synthetic plans and blocked metadata observations have no source bindings, so these rules cannot truthfully filter them. UI must say future scans only and not applied to the example plan. M2 must consume rules before traversal using independently authorized scopes and invalidate affected coverage/revisions. This batch does not close M1B, M0 permission gates or the human pilot.

Work starts on feat/m1b-exclusions based on b3cd202/PR4. Matching versions become0.0.6; no release. Each storage/identity/native/UI batch is independently reviewed and verified on disposable fixtures only.
