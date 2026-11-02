# Purge result visibility and confirmation checks — 2026-11-02

Baseline PR19head572f11d; branch feat/m1c-purge-result-visibility. This is a bounded bug-fix and acceptance-test follow-up at0.0.16, with no new cleanup scope, release or backend authority.

Observed gap: result markup was nested under ready status, but known permanent removal maps demo status to needs_inspection. Real refreshed status could therefore hide the recorded result and volume observation. Move the result outside that condition, retain operation guards, and announce it accessibly. Show exact per-item removed/uncertain/not-attempted outcomes using labels frozen from the approved review rather than mutable current plan metadata. Never infer that uncertain/not-attempted items remain available or reveal raw receipt/attempt IDs.

Behavioral checks must reproduce the real post-purge inspection status, preserve recorded evidence on refresh failure, keep logical/volume accounting separate, and preserve frozen labels after plan rerenders. Native warning and actual coordinator confirmation-boundary tests should prove exact labels/total/irreversibility and fresh refusal after target/plan changes during confirmation, before claim/ledger/unlink/measurement. Use only app-created disposable fixtures.

No milestone completion is claimed; human review/credential and broader platform gates remain open.

Integration baseline: PR19 merged to maind424518; no stacked base remains. The backend tests are test-only additions; production behavior changes only in result presentation.
