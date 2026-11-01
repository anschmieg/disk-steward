# Decision 0002 — M0 offline detector scope

**Status:** Feasibility decision recorded; the M0 scope-freeze gate remains open.  
**Date:** 1 November 2026  
**Release line:** 0.0.4 (no release authorized)

## Decision

For M3 implementation planning, the two generated-data families selected as
executable targets are **D2 CPython checked-hash bytecode** and **D3 one pytest
cache record**. This authorizes future implementation and fixture work only.
Neither detector is enabled in the app now, and these probes are not production
adapters or permission to remove files. M3 still has to demonstrate the full
editable-plan, stage, restore, separately approved purge, and measured-space
lifecycle for representative outputs.

The evidence is deliberately narrow. D2 is pinned to **CPython 3.15.0** and a
standard `__pycache__` `.pyc` using the checked-hash header (`flags == 3`) whose
magic is `520e0d0a` and cache tag is `cpython-315`. The entry must have a
retained regular `.py` source in the same selected project scope, at exactly
the path returned by `cache_from_source`, and its source hash must match. The
probe's regeneration evidence is a byte-for-byte `py_compile` regeneration
using `PycInvalidationMode.CHECKED_HASH` on a fixture whose source raises if
executed. Unknown interpreter/build, timestamp-based or unchecked-hash
bytecode, missing or changed source, custom cache paths/loaders, bytecode-only
modules, symlinks, unexpected candidates, and an unavailable or untrusted
compatible regeneration path are ineligible. The only root for future
discovery is an explicitly user-selected project root; this does not authorize
walking other volumes, environments, package stores, or home-directory caches.

D3 is narrower still: **pytest 9.2.0**'s manager-generated standard
`.pytest_cache/v/cache/nodeids` regular file, UTF-8 JSON containing a non-empty,
sorted, duplicate-free array of non-empty relative node-id strings. pytest's
9.2.0 implementation writes `cache/nodeids` as a sorted string list and uses
the standard cache value directory; its root/cache layout is defined by the
project's pytest root selection and `.pytest_cache` default ([9.2.0
cacheprovider source](https://github.com/pytest-dev/pytest/blob/9.2.0/src/_pytest/cacheprovider.py),
[pytest root-directory documentation](https://docs.pytest.org/en/stable/reference/customize.html#initialization-determining-rootdir-and-configfile)).
The only allowed discovery root is a user-selected project root with a
statically recognized pytest root/configuration and an immediate `.pytest_cache`
child. The exact supported layout is the fixture layout captured by the probe;
custom `cache_dir`, third-party cache records, other pytest versions, malformed
or unknown values, symlinks, and contents outside this one record remain
review-only/blocked. Generated support files (`CACHEDIR.TAG`, `.gitignore`,
`README.md`, and `v/`) can support ownership evidence but are never themselves
removal targets. No recursive removal of `.pytest_cache` is allowed.

pytest cache files do not encode their writer's version. Therefore a
production detector must substantiate pytest 9.2.0 provenance from a
non-executing, project-local environment record tied to the selected root, or
block the finding. It must not execute a project's interpreter, plugin,
configuration hook, test, build, or shell command to discover that provenance.
If that provenance cannot be established without executing project code, D3
must remain review-only at M3 and the minimum two-family gate is not met. The
probe's pinned temporary environment only demonstrates fixture production,
not that any user's cache came from that version.

For both families, **active use or uncertain active-use state blocks staging**.
An active Python process that may import the source/cache pair, an active
pytest process, an unresolved concurrent writer, a race in the activity check,
or lack of a reliable activity check blocks the affected finding. An idle
snapshot from before review is insufficient: the production adapter must
recheck use and item identity immediately before staging and return a visible
blocker on uncertainty. No active-use detector exists in these probes, so this
is an open M3 implementation gate rather than a passed test.

**D1 Cargo output and D4 Homebrew downloads stay review-only** for the planned
M3 slice. No probe here establishes safe ownership, exact configured roots,
manager-version provenance, custom output behavior, installed-state
separation, or individual artifact regeneration/checksum evidence. Cargo
requires workspace metadata without running project hooks and per-file
generated-output provenance; Homebrew requires a validated read-only root
adapter and package checksum evidence. Reconsider either only with new
bounded, manager-generated evidence and a recorded scope decision.

D5 downloaded installers retains its proposed explicit per-file staging target
in MVP-CATALOG.md; it is unimplemented and this probe provides no eligibility
evidence for it. This generated-cache subset decision does not downgrade that
separate target. D7 large files/folders remain discover/review-only unless a
supported category adapter establishes eligibility. D8 remains the separately
planned M4 Git adapter. No operation for these families is enabled here.

## M3 gates and explicit exclusions

This selection gives M3 two candidate generated/cache families; it does not
close the roadmap's required third family, D6 exact duplicates. D6 remains a
hard future M3 gate. Before D6 may be executable, its adapter must prove all
of the following on disposable, fully allocated fixture files: stable
full-content verification for every grouped candidate; identity/size/mtime
revalidation before staging; explicit user selection of the retained survivor;
hard-link groups treated as shared storage rather than duplicate reclaim;
symlinks, cloud placeholders, changing files, unsupported/managed-library
internals and partial-coverage scopes blocked; intersections where a survivor
is also selected for removal rejected; and receipt-based stage/restore plus a
separate purge that measures real recovered space. Hash matches alone are not
identity proof. No D6 feasibility or lifecycle result is claimed by this
decision.

For D2 and D3, unknown files remain in place and are reported separately.
Unexpected siblings do not make an entire directory disposable. The probes
accept no user-supplied paths; future scope comes only from a selected,
authorized project root. Findings need source/root ownership, format and
version evidence, exact item identity, current activity state, and a recovery
receipt before any execution workflow can consider them.

## Evidence gathered

All new fixtures were created under `TemporaryDirectory`; none of the commands
accepted user paths or touched project files. The checked-in M0 evidence also
records an earlier isolated pytest 9.2.0 fixture run. The delegate initially could not replay the optional dependency because
restricted network access failed. The coordinator subsequently installed the
pinned package in an isolated temporary environment with network access,
replayed the updated probe successfully and removed that environment. The
[recorded result](../../probes/catalog/evidence/2026-11-01-fixture-replay.json)
contains the manager-generated node-id value and exact CPython observations.

| Command / observation | Result | Limit |
|---|---|---|
| `python3 --version` | `Python 3.15.0` | One local interpreter/build only |
| `python3 -m unittest discover -s probes/catalog -v` before probe expansion | 3 tests passed | Original negatives: altered bytes, missing source, symlink, unexpected file; all synthetic |
| `python3 probes/catalog/detector_probe.py` before probe expansion | D2 fixture passed: magic `520e0d0a`, `cpython-315`, checked-hash match, exact regeneration, changed/missing/symlink/unexpected rejection, source not executed; pytest unavailable | No user paths; does not prove app discovery, source ownership, concurrency, or staging |
| `python3 -m venv /tmp/disk-steward-catalog-venv && /tmp/disk-steward-catalog-venv/bin/python -m pip install -r probes/catalog/requirements.txt` | Failed after DNS retries to `pypi.org`; no matching version could be retrieved | Temporary venv only; no global package install; present-turn pytest 9.2.0 replay unavailable |
| Existing [M0 evidence](../../probes/M0-EVIDENCE.md) and [catalog probe notes](../../probes/catalog/README.md) | Coordinator records pytest 9.2.0 installed in a disposable environment; fixture ran with plugin autoload disabled and custom cache root; recognized JSON `nodeids` record, no unexpected top-level entries | Output was summarized rather than preserved as a raw artifact; exact bytes/layout were not independently replayed in this turn |
| `python3 -m unittest discover -s probes/catalog -v` after probe expansion | 5 tests passed, including changed source and header-flag refusals plus non-empty sorted nodeids array, malformed JSON/shape, duplicate ID, traversal, unexpected content preservation, symlink, and root escape refusals | Negative records are hand-authored; the coordinator replay below supplies a manager-generated positive |
| `python3 probes/catalog/detector_probe.py` after probe expansion | D2 passed again, including changed-source and unknown-header rejection; D3 explicitly reported unavailable, `pytest is not installed; no install attempted` | The updated D3 path was not exercised against the manager-generated positive artifact in this pass |

| Coordinator temporary venv, pinned pytest 9.2.0, updated probe | Both fixtures passed; nodeids `test_fixture_only.py::test_fixture_value`, no unexpected top-level/cache entries; CPython checked-hash regeneration and all probe negatives passed | Synthetic manager-generated fixture only; temporary environment removed; no user project executed |

The standard-library tests exercise refusal and preservation behavior. They do
not establish production classification, reliable project/version
provenance, race-free active-use checks, complete lifecycle behavior, or real
space recovery. The checked-in fixture result records the coordinator replay; earlier raw
output is not substituted for it.

## Remaining M0 work

M0 remains open. This decision does not freeze the full catalog or forecast and
does not close the other M0 gates in `ROADMAP.md`. Still open here are:

- defining and proving non-executing pytest 9.2.0 provenance for a selected
  user's project or retaining D3 as review-only;
- implementing and proving conservative active-use checks for both targets;
- proving each detector through the full M1 recovery lifecycle and then M3's
  separate approval/purge/space-recovery tests;
- D6's complete exact-duplicate feasibility and lifecycle gate;
- the uncompleted native permission, Git private-credential/resource,
  independent AI evaluation, storage and reforecast gates recorded elsewhere.

The coordinator records this target selection in the catalog/support matrix.
No application execution capability is enabled by this decision.
