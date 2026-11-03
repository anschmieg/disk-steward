# pytest record reference corpus

This probe supports the backend D3 record parser in `crates/generated-evidence`.
It does not scan user projects or validate cleanup eligibility. The two records
are actual outputs from a fixed passing/failing test pair, generated with pytest
9.2.0 and CPython 3.14.10 in a task-owned virtual environment on macOS arm64.
The one intentional failure makes pytest exit 1. Neither tests nor project hooks
from a user directory are loaded. Third-party plugin auto-discovery and conftest loading are disabled.

The cache-provider formats are documented by [pytest](https://docs.pytest.org/en/stable/how-to/cache.html)
and implemented by its [cache provider](https://github.com/pytest-dev/pytest/blob/9.2.0/src/_pytest/cacheprovider.py).
The reference version describes the tested corpus, not a tool version inferred
from arbitrary JSON. An earlier pytest9.0.2 run produced identical bytes;
that corpus is retained to test this limitation explicitly.

To reproduce, provision a disposable virtual environment with pytest==9.2.0,
then choose a new destination under an existing task-owned parent:

```sh
python3 probes/generated_cache/generate_pytest_fixture.py \
  --python /path/to/disposable/venv/bin/python \
  --destination /path/to/task-owned-parent/new-fixture
```

The generator itself installs nothing. It requires at least 2GiB free plus
64MiB headroom, refuses existing destinations and unknown versions, and leaves
partial artifacts for inspection. It never cleans up on exit. Dependency
installation in the coordinating run used `pip --no-cache-dir` in the isolated
venv; no global packages or user caches were changed. The default local
CPython3.15 installation lacked SSL, so dependency installation used the
existing CPython3.14 interpreter instead, preserving TLS validation.

Two independent pytest9.2.0 runs produced the committed SHA256 values in
`fixtures/pytest-9.2.0/evidence.json`. Rust integration tests consume those exact
record bytes; Python refusal tests run in the ordinary probe test discovery
without requiring pytest or an external install. The actual manager run is an
explicit probe, not part of CI, so CI does not establish tool availability.

Only nodeids arrays and lastfailed objects with true values are recognized.
The parser is deliberately conservative and refuses collection-only node IDs,
non-Python collectors, large records, nested JSON, duplicate entries/keys,
unsafe path-like prefixes and unknown values. Unknown cache files, cache-root
markers and project configuration are not accepted through this API. Matching
JSON can be forged: this is format evidence, not ownership or provenance.

Live execution remains disabled. A native collector must bind selected project,
cache and individual file identities through no-follow handles, verify protected
scope/exclusions and consent, bound reads and cancellation, establish ownership,
and refuse uncertain active use. A separate adapter must freeze exact actions,
revalidate after approval, stage recoverably with receipts, restore safely and
require a separate exact approval for permanent removal. The existing five-file
fixture adapter remains unchanged. No M0/M3/MVP completion is claimed.
