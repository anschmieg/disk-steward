# Generated cache feasibility probe

Run on the development interpreter with:

```sh
python3 -m unittest discover -s probes/catalog -v
python3 probes/catalog/detector_probe.py
```

The CPython fixture uses `py_compile` with checked-hash invalidation. Its
source begins with a `raise RuntimeError`, which confirms the probe compiles
but never imports or executes it. It verifies the current interpreter magic
and cache tag, checked source hash, expected `__pycache__`/source pairing, and
byte-for-byte regeneration. Negative fixtures reject changed bytecode, missing
source, changed source, an unknown header flag, symlinks, and unexpected files;
the unit test also verifies an unknown file remains unchanged. All files live
in `TemporaryDirectory` fixtures.

This scope is pinned to CPython 3.15.0, magic `520e0d0a`, and tag `cpython-315`;
other runtimes do not run the probe. This remains feasibility evidence, not an
app detector.

When exactly pytest 9.2.0 is already importable, the probe runs one synthetic
test file with plugin autoload disabled and a cache root inside that temporary
fixture. It validates manager-generated `v/cache/nodeids` as a non-empty,
sorted, duplicate-free JSON array of relative strings, and reports unknown
records without removing anything. Other pytest versions are not run by this
probe. Unit tests add hand-authored negative fixtures for malformed records,
duplicate values, symlinks, escaped paths, and preserved unknown content;
they do not replace a manager-generated positive fixture.

The fixture history for this workspace is: CPython 3.15.0 ran successfully;
the earlier coordinator-observed isolated environment had pytest 9.2.0 and
produced a recognized JSON `nodeids` record. During the scope-freeze pass,
the pinned dependency could not be reinstalled because DNS for `pypi.org`
failed after retries, and the system interpreter still had no pytest. The
coordinator then replayed the updated probe in an isolated pytest 9.2.0
environment with authorized network access; both fixtures passed and the
environment was removed. The result is retained in
[evidence/2026-11-01-fixture-replay.json](evidence/2026-11-01-fixture-replay.json).
The system interpreter still reports pytest unavailable. No global package was
installed. `requirements.txt` pins only the optional development fixture
dependency; pytest is not an app runtime dependency.

The results do not establish broad bytecode eligibility across CPython
versions/builds, custom loaders, namespace packages, altered import paths,
concurrent writers, source ownership, active-use safety, installed pytest
cache layouts, or human-reviewed detector scope. pytest cache files do not
record their writer version; future D3 eligibility needs non-executing,
project-local 9.2.0 provenance or must remain blocked. Active or uncertain use
blocks either detector; proving the production activity check remains open.
Exact regeneration is a feasibility signal, not a production classifier. No
user path is accepted, no global dependency is installed, and no production
cleanup action is supported by this probe alone. See
[`docs/decisions/0002-detector-scope.md`](../../docs/decisions/0002-detector-scope.md)
for the M3 selection and open gates.
