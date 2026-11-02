# Manual label review protocol

`cases.json` contains provisional synthetic route labels authored by Luna. They
are not independent human judgments and do not satisfy the early AI evaluation
gate in the roadmap. This protocol lets a reviewer create a separate,
development-only route annotation set. It does not promote annotations into the
fixture or establish model quality.

## Start a review

From the repository root, choose a new output path and run:

```sh
python3 evaluation/review_labels.py --output /path/to/new-review.jsonl
```

The program requires both standard input and output to be an interactive
terminal. It presents only development case IDs, facts, and evidence IDs. It
does not display provisional labels, unknown fields, or any held-out case. For
each case, enter one of the listed routes, a rationale (up to 2,000 characters),
and a self-reported reviewer identity (up to 200 characters). Route meanings
follow the evaluation harness: `keep` means retain/protect with no further
investigation; `inspect_metadata` means gather or corroborate metadata next;
`run_specialist` means specialist analysis for a known category;
`reasoning_review` means additional agent reasoning, not human review; and
`ask_user` means personal intent or another user decision is needed. Enter `q`
at the route prompt to stop; completed records are flushed as they are entered.
The output is created exclusively, so
an existing path is never overwritten.

To continue a partial review, use the prior file as read-only input and select a
different, new output path:

```sh
python3 evaluation/review_labels.py --resume /path/to/prior.jsonl --output /path/to/continued-review.jsonl
```

Resume validates the complete prior JSONL (at most 60 records, each at most
16,384 encoded bytes), rejects duplicate or conflicting case IDs, invalid
schema/routes, unknown IDs, and held-out IDs, copies valid
records into the new output, and presents only unreviewed development cases.
There is no batch or affirmative auto-review option.

## Meaning and limits of records

Each JSONL record includes a schema version, case ID, route, rationale, reported
reviewer identity, timezone-aware timestamp, SHA-256 digest of the canonical
case ID/facts/evidence IDs, route contract version and route definitions, and the provenance statement that this is a
manual reviewer attestation whose identity is self-reported and not
independently verified. The tool cannot prove who used it or whether a person
reviewed the case. Do not treat its output as cryptographic or identity
verification.

The digest detects changes to the reviewed facts, evidence IDs, or route
contract before a resumed session. It deliberately excludes the provisional
label and unknown fields, which are not shown to the reviewer.

The route choices are annotations for evaluation. None authorizes filesystem
access, cleanup, model calls, or any other action. The tool makes no network
requests, changes no fixture labels, and has no application cleanup authority.
Only development cases are eligible; held-out facts and labels remain reserved
for M5. The output alone does not pass the roadmap's M0 evaluation gate, which
also requires a bounded, consented development evaluation with recorded
measurements for routing, investigation usefulness, evidence adequacy, cost,
and latency. Human-reviewed labels still need coordinator review and agreement
checks before being used as an evaluation reference.
