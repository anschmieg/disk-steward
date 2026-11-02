"""Fixture-only CPython bytecode and optional pytest-cache feasibility probe.

This module has no arbitrary-path command interface. Every filesystem object
it classifies is created by ``run_fixture_probe`` under a TemporaryDirectory.
It is development evidence, not a production cleanup adapter.
"""
from __future__ import annotations

import importlib.util
import json
import os
import py_compile
import struct
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


class FixtureRejected(RuntimeError):
    """The fixture is outside the narrow, validated generated-file shape."""


def _within(root: Path, path: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def validate_bytecode(root: Path, source: Path, candidate: Path,
                       expected_bytes: bytes) -> dict[str, Any]:
    """Validate one exact regenerated fixture .pyc and its retained source."""
    # Compare lexical absolute paths first, then check every component inside
    # the fixture root. Ignore ancestors above root (for example macOS /var is
    # a system symlink to /private/var, outside the fixture's control).
    root = Path(os.path.abspath(root))
    source = Path(os.path.abspath(source))
    candidate = Path(os.path.abspath(candidate))
    if not _within(root, source) or not _within(root, candidate):
        raise FixtureRejected("path escaped the fixture root")
    for path in (source, candidate):
        current = path
        while current != root:
            if current.is_symlink():
                raise FixtureRejected("symbolic links are not eligible")
            current = current.parent
        if root.is_symlink():
            raise FixtureRejected("fixture root is a symbolic link")
    if not source.is_file() or source.suffix != ".py":
        raise FixtureRejected("retained Python source is missing or not a regular .py file")
    if not candidate.is_file() or candidate.suffix != ".pyc":
        raise FixtureRejected("candidate is not a regular .pyc file")
    expected_path = Path(importlib.util.cache_from_source(str(source)))
    if candidate != expected_path:
        raise FixtureRejected("bytecode path does not pair with the retained source")

    actual = candidate.read_bytes()
    if len(actual) < 16 or actual[:4] != importlib.util.MAGIC_NUMBER:
        raise FixtureRejected("bytecode magic/format is incompatible with this interpreter")
    flags = struct.unpack("<I", actual[4:8])[0]
    if flags != 3:
        raise FixtureRejected("only checked-hash pyc headers are accepted by this probe")
    source_bytes = source.read_bytes()
    if actual[8:16] != importlib.util.source_hash(source_bytes):
        raise FixtureRejected("bytecode source hash does not match retained source")
    if actual != expected_bytes:
        raise FixtureRejected("bytecode differs from the exact regenerated fixture")
    return {
        "kind": "cpython_checked_hash_bytecode",
        "magic": actual[:4].hex(),
        "cache_tag": sys.implementation.cache_tag,
        "source": source.name,
        "size": len(actual),
        "regeneration_match": True,
    }


def regenerate(source: Path) -> tuple[Path, bytes]:
    cache = Path(py_compile.compile(
        str(source), doraise=True,
        invalidation_mode=py_compile.PycInvalidationMode.CHECKED_HASH,
    ))
    return cache, cache.read_bytes()


def _pytest_cache_probe(root: Path) -> dict[str, Any]:
    """Use installed pytest only, in an internally-created project fixture."""
    if importlib.util.find_spec("pytest") is None:
        return {"status": "unavailable", "reason": "pytest is not installed; no install attempted"}

    project = root / "pytest-fixture"
    project.mkdir()
    test_file = project / "test_fixture_only.py"
    test_file.write_text("def test_fixture_value():\n    assert 2 + 2 == 4\n", encoding="utf-8")
    cache_dir = project / ".pytest_cache"
    env = os.environ.copy()
    env["PYTEST_DISABLE_PLUGIN_AUTOLOAD"] = "1"
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    command = [sys.executable, "-m", "pytest", "-q", "-p", "cacheprovider",
               "-o", f"cache_dir={cache_dir}", str(test_file)]
    try:
        result = subprocess.run(command, cwd=project, env=env, text=True,
                                capture_output=True, timeout=20, check=False)
    except subprocess.TimeoutExpired:
        return {"status": "inconclusive", "reason": "fixture pytest run exceeded 20 seconds"}
    if result.returncode != 0:
        return {"status": "inconclusive", "returncode": result.returncode,
                "stderr_tail": result.stderr[-1000:]}

    # Inspect known generated cache records as evidence only. Unknown files are
    # retained and reported, never treated as expendable cache material.
    known = {".gitignore", "README.md", "CACHEDIR.TAG", "v"}
    unknown = sorted(p.name for p in cache_dir.iterdir() if p.name not in known)
    records: dict[str, str] = {}
    cache_records = cache_dir / "v" / "cache"
    if cache_records.is_dir() and not cache_records.is_symlink():
        for name in ("nodeids", "lastfailed", "stepwise"):
            record = cache_records / name
            if record.is_file() and not record.is_symlink():
                try:
                    json.loads(record.read_text(encoding="utf-8"))
                    records[name] = "valid-json-fixture-record"
                except (UnicodeError, json.JSONDecodeError):
                    records[name] = "invalid-or-unknown; keep"
    return {"status": "fixture-passed", "pytest_version": _pytest_version(),
            "recognized_records": records, "unexpected_top_level_entries": unknown,
            "cache_root": ".pytest_cache (fixture only)"}


def _pytest_version() -> str | None:
    try:
        import pytest  # type: ignore[import-not-found]
        return pytest.__version__
    except Exception:
        return None


def run_fixture_probe() -> dict[str, Any]:
    """Create, compile, classify and negatively mutate only disposable fixtures."""
    with tempfile.TemporaryDirectory(prefix="disk-steward-catalog-") as temp:
        root = Path(temp)
        source = root / "fixture_module.py"
        # Compiling this source is safe and observable: executing it would fail.
        source.write_text("raise RuntimeError('probe source must never execute')\nVALUE = 7\n",
                          encoding="utf-8")
        cache, first_bytes = regenerate(source)
        accepted = validate_bytecode(root, source, cache, first_bytes)

        cache.unlink()
        regenerated_cache, regenerated_bytes = regenerate(source)
        exact_regeneration = first_bytes == regenerated_bytes
        if not exact_regeneration:
            raise FixtureRejected("checked-hash bytecode was not reproducible")
        accepted = validate_bytecode(root, source, regenerated_cache, first_bytes)

        changed_rejected = False
        pristine = regenerated_cache.read_bytes()
        regenerated_cache.write_bytes(pristine[:-1] + bytes([pristine[-1] ^ 1]))
        try:
            validate_bytecode(root, source, regenerated_cache, first_bytes)
        except FixtureRejected:
            changed_rejected = True
        regenerated_cache.write_bytes(pristine)

        missing_source_rejected = False
        source_backup = source.read_bytes()
        source.unlink()
        try:
            validate_bytecode(root, source, regenerated_cache, first_bytes)
        except (FixtureRejected, FileNotFoundError):
            missing_source_rejected = True
        source.write_bytes(source_backup)

        symlink_rejected = False
        symlink = root / "linked.pyc"
        symlink.symlink_to(regenerated_cache)
        try:
            validate_bytecode(root, source, symlink, first_bytes)
        except FixtureRejected:
            symlink_rejected = True

        unexpected_rejected = False
        unexpected = root / "__pycache__" / "notes.txt"
        unexpected.parent.mkdir(exist_ok=True)
        unexpected.write_text("user-kept note fixture", encoding="utf-8")
        try:
            validate_bytecode(root, source, unexpected, first_bytes)
        except FixtureRejected:
            unexpected_rejected = True

        pytest_result = _pytest_cache_probe(root)
        return {
            "cpython": {
                "status": "fixture-passed",
                "python_version": sys.version.split()[0],
                "cache_tag": sys.implementation.cache_tag,
                "magic": importlib.util.MAGIC_NUMBER.hex(),
                "compatible_source_pair": accepted["source"] == source.name,
                "checked_hash_source_match": True,
                "regeneration_exact_match": exact_regeneration,
                "changed_cache_refused": changed_rejected,
                "missing_source_refused": missing_source_rejected,
                "symlink_refused": symlink_rejected,
                "unexpected_file_refused": unexpected_rejected,
                "source_not_executed": True,
            },
            "pytest": pytest_result,
            "scope": "synthetic fixtures only; no user paths, no global installation",
        }


if __name__ == "__main__":
    print(json.dumps(run_fixture_probe(), indent=2, sort_keys=True))
