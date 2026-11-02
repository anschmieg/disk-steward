# Fixture lifecycle migration probe

This standalone test crate exercises a proposed v1-to-v2 SQLite schema migration against disposable fixture sessions created by `disk-steward-fixture-lifecycle`. It accepts no paths or runtime arguments and contains no file-removal operation. Its v2 schema is experimental; it is not a production migration or purge contract.

Run with `CARGO_TARGET_DIR=/private/tmp/disk-steward-migration-target cargo test --manifest-path probes/fixture-migration/Cargo.toml`.
