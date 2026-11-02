# Offline evaluation fixtures

`cases.json` contains 100 synthetic, Luna-authored provisional labels with no real filesystem paths or personal data. Independent human label review remains required before these can satisfy the live M0 evaluation gate. The case mix is 40 generated/cache, 20 Git, 20 personal/duplicate, and 20 ambiguous/adversarial. Whole scenario families are assigned to one split: 60 development and 40 held-out.

Run the offline checks from the repository root:

```sh
python3 evaluation/harness.py --score-baseline --check-validator
```

The default deterministic baseline scores routing on development cases only. Held-out labels are count-checked for split integrity but are not scored. `validate_response` checks response shape, route, evidence references, next-investigation vocabulary, and that proposed action lists are empty. It never authorizes an operation.

These fixtures and mocks validate data shape, deterministic fixture behavior, and rejection paths only. They cannot establish model usefulness, real-world routing quality, evidence adequacy, cost, latency, or safety. The apparent 100% deterministic baseline is expected because labels are constructed from matching conservative rules; it is not an independent benchmark. The M0 live development evaluation still requires explicit credentials and consent, bounded requests and spend, and recorded measurements. The held-out set must be reserved for M5 evaluation.

## Optional Jev 1.14 preliminary probe

`live_jev.py` defaults to dry-run and sends nothing unless `--live` is explicit.
It can select one or more development cases by `--case-id` (repeat the option,
maximum five), or take the first cases with `--max-requests`. A requested held-out
or unknown ID is rejected. Requests contain only each selected synthetic case's
facts and evidence IDs; labels and held-out facts are never sent. The API key is
read from `OPENROUTER_API_KEY` only in live mode.

The choice labels have distinct, non-mutating meanings: `keep` means affirmatively
retain/protect with no further investigation; `inspect_metadata` means gather or
corroborate metadata next; `run_specialist` means analysis by a specialist for a
known category; `reasoning_review` means additional agent reasoning (not human
review); and `ask_user` means personal intent or another user decision is needed.
No route authorizes cleanup or is interpreted as a cleanup command.

For example, inspect the exact one-case payload offline with:

```sh
python3 evaluation/live_jev.py --case-id gen-cargo-001
```

A one-case live result, including the successful feasibility request, is only a
preliminary connectivity/schema/cost observation. It does not establish model
quality, routing accuracy, evidence adequacy, label agreement, or the M0
evaluation gate. Fixture labels remain provisional and require independent human
review. Any reported cost threshold is checked after a response arrives; it is
not a hard in-flight billing cap. Live results are marked unreviewed and the
runner stores only the synthetic case ID, selected route, reported cost,
response ID, and model.

## Independent development-label review

A blind, terminal-only manual review helper is available; see [review protocol](REVIEW-PROTOCOL.md). It stores annotations separately, never changes provisional labels, excludes held-out scenarios, and binds each annotation to the exact facts/evidence and route definitions shown. Reviewer identity is self-reported, not verified. No human annotations have been collected by this implementation. Completing the helper does not by itself establish model usefulness or pass M0.
