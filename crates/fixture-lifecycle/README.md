# Disposable fixture lifecycle

The original profile creates exactly five synthetic files beneath an internally named private session directory. A separate closed pytest 9.2.0 profile creates two fixed copied corpus records and uses the same durable lifecycle engine; see [its profile contract](../../docs/contracts/pytest-corpus-recovery-profile-v1.md). It accepts a trusted parent only at construction; stage and restore accept typed action, receipt and attempt identifiers. It has no operation for adopting user files, accepting renderer paths, or recursively deleting a tree. Its low-level purge method accepts only an exact current staged receipt for a registered app-owned file; approval belongs to the caller’s validated coordinator.

The coordinator records a creation intent before calling `create_for_session`. Partial creation is retained for inspection. The journal binds fixed file slots, expected content and file/directory identities. Stage moves a registered source into the same-volume holding directory with no overwrite; restore moves a registered receipt back only if the original name is free. Both retain data and recover zero bytes.

Pending intents and ambiguous results remain uncertain after reopening. The component does not retry or roll back. Application review, native confirmation, approval consumption and whole-service ownership belong to the execution coordinator; this component alone grants none of those permissions.

Run from the repository root:

```sh
cargo test -p disk-steward-fixture-lifecycle
```

Tests use disposable temporary directories, including content changes, occupied destinations, interrupted journal states, concurrent claims, symlink replacement and malformed registries. macOS tests have passed locally; other operating systems retain their separate validation gates. This does not enable a production cleanup adapter.
