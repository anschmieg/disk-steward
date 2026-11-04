# Generated-data evidence

A backend-only, in-memory D3 pytest cache format classifier. The API accepts a
closed record kind and at most 64KiB of bytes; it never accesses files, resolves
paths, invokes tools, changes plans or authorizes execution. At most512 unique
records and1024 UTF8 bytes per node ID are accepted. JSON nesting is checked
before deserialization. Duplicate keys are rejected before any map overwrite.
Names and bytes do not appear in the returned count/version evidence.

`reference_version` names the pinned fixture producer against which format
recognition was tested. It is **not** a detected installed or producing version.
Format recognition cannot establish ownership, idleness, regeneration, freshness
or cleanup eligibility. Empty records are valid formats, not useful cleanup
findings. A conservative subset of Python `relative/file.py::test` node IDs is
accepted; unsupported collectors and collection-only records refuse.

See [reference corpus and remaining execution gates](../../probes/generated_cache/README.md).
Run `cargo test -p disk-steward-generated-evidence --locked` and strict all-target
Clippy. The crate is included in ordinary workspace CI; the explicit manager
probe requires a separately provisioned pinned virtual environment.
