# Offline evaluation fixtures

`cases.json` contains 100 synthetic, Luna-authored provisional labels with no real filesystem paths or personal data. Independent human label review remains required before these can satisfy the live M0 evaluation gate. The case mix is 40 generated/cache, 20 Git, 20 personal/duplicate, and 20 ambiguous/adversarial. Whole scenario families are assigned to one split: 60 development and 40 held-out.

Run the offline checks from the repository root:

```sh
python3 evaluation/harness.py --score-baseline --check-validator
```

The default deterministic baseline scores routing on development cases only. Held-out labels are count-checked for split integrity but are not scored. `validate_response` checks response shape, route, evidence references, next-investigation vocabulary, and that proposed action lists are empty. It never authorizes an operation.

These fixtures and mocks validate data shape, deterministic fixture behavior, and rejection paths only. They cannot establish model usefulness, real-world routing quality, evidence adequacy, cost, latency, or safety. The apparent 100% deterministic baseline is expected because labels are constructed from matching conservative rules; it is not an independent benchmark. The M0 live development evaluation still requires explicit credentials and consent, bounded requests and spend, and recorded measurements. The held-out set must be reserved for M5 evaluation.
