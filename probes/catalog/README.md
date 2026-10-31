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
source, symlinks, and unexpected files; the unit test also verifies an unknown
file remains unchanged. All files live in `TemporaryDirectory` fixtures.

On this machine (CPython 3.15.0, cache tag `cpython-315`), the positive and all
negative CPython cases passed. pytest was not installed, so the probe reported
D3 unavailable and did not install it. When pytest is already importable, the
probe runs only one synthetic test file with plugin autoload disabled and a
cache root inside the same temporary fixture, then reports known JSON records
and unknown entries without removing anything.

These results establish only that this fixture technique works for the
current interpreter. They do not establish broad bytecode eligibility across
CPython versions/builds, custom loaders, namespace packages, altered import
paths, concurrent writers, source ownership, active-use safety, installed
pytest cache layouts, or human-reviewed detector scope. Exact regeneration is
a feasibility signal here, not a production classifier. No user path is
accepted, no pytest/global dependency is installed, and no production cleanup
action is supported by this probe alone.

Coordinator follow-up: an isolated disposable virtual environment with pytest 9.2.0 ran the fixture successfully and produced a validated JSON `nodeids` record with no unknown top-level entries. The installed-system interpreter still reports pytest unavailable; no global package was installed. Optional reproducibility dependencies are pinned in requirements.txt. Cache-root configuration uses pytest's `-o cache_dir=...` setting. This establishes fixture feasibility only.
