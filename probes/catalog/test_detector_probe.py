from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from probes.catalog.detector_probe import (
    FixtureRejected,
    _pytest_cache_probe,
    regenerate,
    run_fixture_probe,
    validate_bytecode,
    validate_pytest_nodeids,
)


class DetectorFixtureTests(unittest.TestCase):
    def test_cpython_fixture_positive_and_negative_cases(self) -> None:
        result = run_fixture_probe()
        cpython = result["cpython"]
        if cpython["status"] == "unsupported-version":
            self.skipTest("the pinned CPython 3.15.0 fixture is unavailable")
        for key in (
            "compatible_source_pair", "checked_hash_source_match",
            "regeneration_exact_match", "changed_cache_refused",
            "changed_source_refused", "unknown_header_refused",
            "missing_source_refused", "symlink_refused",
            "unexpected_file_refused", "source_not_executed",
        ):
            with self.subTest(evidence=key):
                self.assertTrue(cpython[key])
        self.assertEqual(result["scope"], "synthetic fixtures only; no user paths, no global installation")

    def test_unexpected_file_is_refused_and_preserved(self) -> None:
        with tempfile.TemporaryDirectory(prefix="disk-steward-catalog-test-") as temp:
            root = Path(temp)
            source = root / "module.py"
            source.write_text("VALUE = 1\n", encoding="utf-8")
            cache, baseline = regenerate(source)
            unknown = cache.parent / "notes.txt"
            unknown.write_text("keep this unknown file", encoding="utf-8")

            with self.assertRaises(FixtureRejected):
                validate_bytecode(root, source, unknown, baseline)

            self.assertEqual(unknown.read_text(encoding="utf-8"), "keep this unknown file")

    def test_pytest_probe_never_installs_missing_dependency(self) -> None:
        # The main probe itself decides whether it can run pytest. This verifies
        # the availability check has no installation side effect or path input.
        before = importlib.util.find_spec("pytest") is not None
        result = run_fixture_probe()["pytest"]
        if result["status"] == "not-run":
            self.skipTest("the pinned CPython fixture is unavailable; pytest was not run")
        after = importlib.util.find_spec("pytest") is not None
        self.assertEqual(before, after)
        if not before:
            self.assertEqual(result["status"], "unavailable")
            self.assertIn("no install attempted", result["reason"])
        else:
            self.assertIn(result["status"], {"fixture-passed", "inconclusive", "unsupported-version"})

    def test_unknown_pytest_version_never_executes_the_fixture(self) -> None:
        with tempfile.TemporaryDirectory(prefix="disk-steward-pytest-version-") as temp:
            with (patch("probes.catalog.detector_probe.importlib.util.find_spec", return_value=object()),
                  patch("probes.catalog.detector_probe._pytest_version", return_value="9.1.0"),
                  patch("probes.catalog.detector_probe.subprocess.run", side_effect=AssertionError("must not run"))):
                result = _pytest_cache_probe(Path(temp))
            self.assertEqual(result["status"], "unsupported-version")
            self.assertEqual(list(Path(temp).iterdir()), [])

    def test_pytest_nodeids_record_shape_and_negative_fixtures(self) -> None:
        with tempfile.TemporaryDirectory(prefix="disk-steward-pytest-cache-") as temp:
            root = Path(temp)
            cache = root / ".pytest_cache"
            record = cache / "v" / "cache" / "nodeids"
            record.parent.mkdir(parents=True)
            original = '["test_fixture.py::test_value"]\n'
            record.write_text(original, encoding="utf-8")
            self.assertEqual(validate_pytest_nodeids(root, cache, record)["count"], 1)

            for payload in ("{\"not\": \"an array\"}\n", "not json\n",
                            '["test_fixture.py::test_value", "test_fixture.py::test_value"]\n',
                            '["../outside.py::test_value"]\n',
                            '["test_fixture.py"]\n'):
                with self.subTest(payload=payload):
                    record.write_text(payload, encoding="utf-8")
                    with self.assertRaises(FixtureRejected):
                        validate_pytest_nodeids(root, cache, record)

            record.write_text(original, encoding="utf-8")
            unexpected = cache / "v" / "cache" / "user-notes.txt"
            unexpected.write_text("retain this unknown cache content", encoding="utf-8")
            with self.assertRaises(FixtureRejected):
                validate_pytest_nodeids(root, cache, unexpected)
            self.assertEqual(unexpected.read_text(encoding="utf-8"),
                             "retain this unknown cache content")

    def test_pytest_nodeids_symlink_and_escape_are_refused(self) -> None:
        with tempfile.TemporaryDirectory(prefix="disk-steward-pytest-cache-") as temp:
            root = Path(temp)
            outside = root.parent / (root.name + "-outside")
            outside.write_text('["test_fixture.py::test_value"]\n', encoding="utf-8")
            cache = root / ".pytest_cache"
            record = cache / "v" / "cache" / "nodeids"
            record.parent.mkdir(parents=True)
            record.symlink_to(outside)
            try:
                with self.assertRaises(FixtureRejected):
                    validate_pytest_nodeids(root, cache, record)
                with self.assertRaises(FixtureRejected):
                    validate_pytest_nodeids(root, cache, outside)
            finally:
                outside.unlink(missing_ok=True)


if __name__ == "__main__":
    unittest.main()
