# Closed pytest corpus recovery profile

The fixture lifecycle has a backend-only profile for the two committed pytest
9.2.0 corpus records. Its factory creates new private files from fixed embedded
bytes. It does not open or adopt project records, accept arbitrary bytes or
filenames, or grant renderer execution access. Creation checks a 2 GiB free-space
reserve plus 64 MiB headroom on the parent volume before creating the session;
this sample cannot reserve space against other writers. The records are copied manager
output, not evidence of a live project's ownership, idle state or regeneration.

The profile uses the existing stage, restore, receipt purge and journal
validation engine. Its durable discriminator must identify the exact closed
registry independently of mutable fixture rows. A reopen validates the schema,
profile, bound directories, exact record names, sizes, digests, action IDs and
attempt history before returning state. Reopen never retries a pending intent.
Unknown profiles, mixed registries and altered corpus fail closed. Historical
synthetic sessions retain their existing schema and migration behavior.

The existing five-file coordinator entry point and synthetic purge-review
service explicitly reject this profile. A separate backend corpus entry point
now binds its own fixed editable plan, exact review, single-use approval and
journaled stage/restore/separate-purge flow; see [the execution contract](pytest-corpus-execution-v1.md).
The synthetic manifests and native confirmation wording do not authorize corpus
records. The low-level lifecycle remains trusted backend infrastructure; its
methods do not themselves obtain user approval. Live detector evidence and
native product approval integration remain required before exposing live
project mutations through a UI.

Tests use disposable app-owned parents. They exercise reopen across lifecycle
transitions and refusal paths, preserving unselected records and unknown
sentinels. They do not establish native user interaction, live-source race
safety, power-loss durability, measured recovered space or milestone completion.
The trusted parent ownership and single-coordinator requirements remain in
force; this is no arbitrary-path cleanup interface.
