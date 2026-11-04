# Approved pytest corpus execution v1

This backend-only contract connects the two fixed pytest 9.2.0 corpus records to
an editable Keep/Include plan and the existing durable execution coordinator.
It accepts no project source paths, arbitrary filenames, caller-supplied bytes or
live inventory entries. The corpus consists of new app-owned copies, not adopted
user files.

The plan, execution control, approval ledger and purge control use distinct
profile stores. The plan and lifecycle profiles must agree before preparing or
executing a review. Stage/restore manifests bind the corpus contract, adapter
version, plan revision, exact file identities and contents, selected actions,
scope, lifetime and frozen review. A frozen plan remains evidence; the trusted
confirmation flow must obtain and consume a separate single-use approval before
recording an execution intent and calling the lifecycle.

Permanent removal has a separate exact receipt review. Its kind is
`pytest_corpus_purge_review`, and its private manifest adapter version is 1001;
the synthetic adapter remains version 2. The service checks its explicit profile
against the persisted control identity, plan profile and lifecycle profile.
Changed kinds, substituted receipts, cross-profile stores and stale revisions
cannot reuse an earlier approval. Declined confirmation leaves records staged.
Known permanent removal invalidates restore for that receipt.

The owner lease, durable clock guard, per-item journal and uncertainty handling
remain required. Reopening never retries an incomplete mutation. Separate
SQLite stores do not provide an atomic power-loss transaction across the whole
flow; ambiguous terminal writes remain a recovery blocker.

Validation uses disposable copied corpus and trusted confirmation callbacks.
No native dialog or live cleanup command is exposed by this contract. Live
pytest support still requires validated project ownership, active-use and
regeneration evidence, source-race protections, native approval integration and
controlled-space acceptance. Passing corpus tests cannot close those gates.
